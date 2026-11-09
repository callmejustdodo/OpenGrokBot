//! Owner: B. The asker's side of the peer protocol.
//!
//! [`PeerClient`] implements the [`Peers`] port over plain HTTP to each
//! teammate's peer server (Tailscale encrypts the link, so there is no TLS of
//! our own), and keeps one `WS /v1/events` connection per teammate:
//!
//! - Events from a teammate's Mac go to [`Peers::subscribe`] listeners. A Mac
//!   speaks only for itself: presence about another member, updates for another
//!   owner's questions or requests, and roster changes from anyone but the
//!   creator are dropped.
//! - The connection opening and closing is presence ([`super::presence`]). While
//!   it is closed the client reconnects with backoff.
//! - A question or request the teammate's Mac does not take because it cannot be
//!   reached goes to the [`Outbox`] and the call returns `peer_offline`. When the
//!   teammate is back (their events connection reopens) the outbox sends it. If
//!   their Mac then refuses it (for example `agent_not_shared`, US-018), the
//!   refusal reaches listeners as a `question_update` or `request_update` with
//!   status `failed`; a delivered request arrives as a `request_update`; a
//!   delivered question is published by the owner's Mac from then on.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use formation_proto::ask::{AskTarget, Question, QuestionStatus};
use formation_proto::common::{ApiError, ErrorCode, MemberId, QuestionId, RequestId};
use formation_proto::peer::{
    AgentsResponse, PATH_AGENTS, PATH_ASK, PATH_EVENTS, PATH_REQUESTS, PATH_TEAM, PATH_TEAM_JOIN,
    PEER_PORT, PeerAsk, PeerAskAccepted, PeerErrorBody, PeerEvent, PeerJoin, PeerJoinAccepted,
    PeerQuestionAction, PeerRequestCreate, PresenceEvent, RosterResponse, SharedAgentView,
};
use formation_proto::policy::MAX_CHAIN_DEPTH;
use formation_proto::request::{RequestStatus, WorkRequest};
use formation_proto::team::{Member, MemberRole};

use super::outbox::{
    Canceled, Deliver, Delivered, Delivery, Outbox, OutboxEntry, OutboxKey, OutboxOutcome,
    OutboxPayload, OutboxStore, RetryPolicy, lock, now, stopped,
};
use super::presence::PresenceBook;
use crate::ports::{Peers, Result};

/// The roster as this Mac knows it, and where each teammate's peer server is.
/// formationd backs it with the team store and Tailscale (a member record holds
/// the node's stable ID, not an address); tests use a fixed table.
pub trait PeerDirectory: Send + Sync {
    /// This Mac's member, once in a team.
    fn me(&self) -> Option<MemberId>;
    /// Members not removed, this Mac's own included.
    fn members(&self) -> Vec<Member>;
    /// `host:port` of the member's peer server, such as `100.101.102.103:7413`.
    fn address(&self, member: MemberId) -> Option<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerClientConfig {
    /// A teammate's Mac that does not accept the connection within this counts as
    /// offline.
    pub connect_timeout: Duration,
    /// Every HTTP call, end to end. The peer endpoints all answer at once (asks
    /// return `202` before answering).
    pub request_timeout: Duration,
    /// The outbox's retries.
    pub retry: RetryPolicy,
    /// Reconnects of the events connection.
    pub reconnect: RetryPolicy,
    /// An events connection silent this long (the server pings every 15 s) is
    /// treated as lost.
    pub idle_timeout: Duration,
}

impl Default for PeerClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(10),
            retry: RetryPolicy::default(),
            reconnect: RetryPolicy::default(),
            idle_timeout: Duration::from_secs(45),
        }
    }
}

/// The [`Peers`] port over the tailnet. Dropping it stops its tasks; the outbox
/// store keeps what was waiting.
pub struct PeerClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    directory: Arc<dyn PeerDirectory>,
    http: Arc<Http>,
    outbox: Outbox,
    config: PeerClientConfig,
    events: broadcast::Sender<PeerEvent>,
    presence: Mutex<PresenceBook>,
    links: Mutex<HashMap<MemberId, Link>>,
    stop: watch::Sender<bool>,
}

struct Link {
    nudge: Arc<Notify>,
    task: JoinHandle<()>,
}

impl PeerClient {
    pub fn new(
        directory: Arc<dyn PeerDirectory>,
        store: Arc<dyn OutboxStore>,
        config: PeerClientConfig,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            // Never route tailnet traffic through a system proxy.
            .no_proxy()
            // A teammate's formationd may restart between calls; a fresh
            // connection per call never trips on a stale pooled one.
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("HTTP client: {e}")))?;
        let http = Arc::new(Http {
            client,
            directory: directory.clone(),
        });
        let outbox = Outbox::new(store, http.clone(), config.retry);
        let (events, _) = broadcast::channel(1024);
        let (stop, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(ClientInner {
                directory,
                http,
                outbox,
                config,
                events,
                presence: Mutex::new(PresenceBook::default()),
                links: Mutex::new(HashMap::new()),
                stop,
            }),
        })
    }

    /// Starts the outbox (sending what the store kept) and connects to every
    /// teammate's events. Call once, inside the tokio runtime.
    pub fn start(&self) -> Result<()> {
        let outcomes = self.inner.outbox.subscribe();
        tokio::spawn(forward_outcomes(self.inner.clone(), outcomes));
        self.inner.outbox.start()?;
        self.sync_members();
        Ok(())
    }

    /// Re-reads the roster from the directory: connects to new teammates and
    /// drops removed ones. Call after the roster changes.
    pub fn sync_members(&self) {
        let me = self.inner.directory.me();
        let wanted: Vec<MemberId> = self
            .inner
            .directory
            .members()
            .into_iter()
            .map(|member| member.id)
            .filter(|id| Some(*id) != me)
            .collect();
        let mut links = lock(&self.inner.links);
        let mut gone = Vec::new();
        links.retain(|member, link| {
            let keep = wanted.contains(member);
            if !keep {
                link.task.abort();
                gone.push(*member);
            }
            keep
        });
        for member in wanted {
            links.entry(member).or_insert_with(|| {
                let nudge = Arc::new(Notify::new());
                let task = tokio::spawn(run_link(self.inner.clone(), member, nudge.clone()));
                Link { nudge, task }
            });
        }
        drop(links);
        let mut presence = lock(&self.inner.presence);
        for member in gone {
            presence.forget(member);
        }
    }

    /// Reconnects to the member's events now instead of after the backoff.
    pub fn reconnect(&self, member: MemberId) {
        self.inner.reconnect(member);
    }

    /// Last known presence of each teammate.
    pub fn presence(&self) -> Vec<PresenceEvent> {
        lock(&self.inner.presence).all()
    }

    pub fn is_online(&self, member: MemberId) -> bool {
        lock(&self.inner.presence).is_online(member)
    }

    pub fn outbox(&self) -> &Outbox {
        &self.inner.outbox
    }

    /// Stops the events connections and the outbox.
    pub fn shutdown(&self) {
        self.inner.stop.send_replace(true);
        self.inner.outbox.shutdown();
        for link in lock(&self.inner.links).values() {
            link.task.abort();
        }
    }
}

impl Drop for PeerClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[async_trait]
impl Peers for PeerClient {
    async fn join(&self, creator_address: &str, join: PeerJoin) -> Result<PeerJoinAccepted> {
        let url = format!("http://{}{PATH_TEAM_JOIN}", peer_address(creator_address));
        Ok(self.inner.http.call(Method::POST, url, Some(&join)).await?)
    }

    async fn roster(&self, member: MemberId) -> Result<Vec<Member>> {
        let url = self.inner.http.url(member, PATH_TEAM)?;
        let roster: RosterResponse = self.inner.http.call(Method::GET, url, NO_BODY).await?;
        Ok(roster.members)
    }

    async fn agents(&self, member: MemberId) -> Result<Vec<SharedAgentView>> {
        let url = self.inner.http.url(member, PATH_AGENTS)?;
        let agents: AgentsResponse = self.inner.http.call(Method::GET, url, NO_BODY).await?;
        Ok(agents.agents)
    }

    async fn ask(&self, member: MemberId, ask: PeerAsk) -> Result<PeerAskAccepted> {
        let key = OutboxKey::Question(ask.question_id);
        if self.inner.must_queue(member, key) {
            return self.inner.queue(member, OutboxPayload::Ask { ask });
        }
        match self.inner.http.ask(member, &ask).await {
            Ok(accepted) => Ok(accepted),
            Err(CallError::Rejected(error)) => Err(error),
            Err(CallError::Unreachable(_)) => self.inner.queue(member, OutboxPayload::Ask { ask }),
        }
    }

    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question> {
        let key = OutboxKey::Question(question_id);
        if self.inner.outbox.contains(key) {
            if action != PeerQuestionAction::Cancel {
                return Err(not_delivered());
            }
            if let Canceled::Removed(entry) = self.inner.outbox.cancel(key)? {
                return self
                    .inner
                    .local_question(&entry, QuestionStatus::Canceled, None);
            }
            // Being sent right now: cancel it on the owner's Mac.
        }
        let path = format!("/v1/questions/{question_id}/action");
        let url = self.inner.http.url(member, &path)?;
        Ok(self
            .inner
            .http
            .call(Method::POST, url, Some(&action))
            .await?)
    }

    async fn create_request(
        &self,
        member: MemberId,
        request: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        let key = OutboxKey::Request(request.request_id);
        if self.inner.must_queue(member, key) {
            return self.inner.queue(member, OutboxPayload::Request { request });
        }
        match self.inner.http.create_request(member, &request).await {
            Ok(created) => Ok(created),
            Err(CallError::Rejected(error)) => Err(error),
            Err(CallError::Unreachable(_)) => {
                self.inner.queue(member, OutboxPayload::Request { request })
            }
        }
    }

    async fn request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        if self.inner.outbox.contains(OutboxKey::Request(id)) {
            return Err(not_delivered());
        }
        let url = self
            .inner
            .http
            .url(member, &format!("{PATH_REQUESTS}/{id}"))?;
        Ok(self.inner.http.call(Method::GET, url, NO_BODY).await?)
    }

    async fn cancel_request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        let key = OutboxKey::Request(id);
        if let Canceled::Removed(entry) = self.inner.outbox.cancel(key)? {
            return self
                .inner
                .local_request(&entry, RequestStatus::Canceled, None);
        }
        let url = self
            .inner
            .http
            .url(member, &format!("{PATH_REQUESTS}/{id}/cancel"))?;
        Ok(self.inner.http.call(Method::POST, url, NO_BODY).await?)
    }

    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.inner.events.subscribe()
    }
}

impl ClientInner {
    /// A call for a member with calls already waiting queues behind them, so the
    /// member's calls arrive in order; a repeat of a waiting call stays one entry.
    fn must_queue(&self, member: MemberId, key: OutboxKey) -> bool {
        self.outbox.has_pending(member) || self.outbox.contains(key)
    }

    fn queue<T>(&self, member: MemberId, payload: OutboxPayload) -> Result<T> {
        self.outbox.push(member, payload)?;
        Err(ApiError::new(
            ErrorCode::PeerOffline,
            "the teammate's Mac is offline; the call waits and is sent when it is back",
        ))
    }

    fn reconnect(&self, member: MemberId) {
        if let Some(link) = lock(&self.links).get(&member) {
            link.nudge.notify_one();
        }
    }

    fn is_creator(&self, member: MemberId) -> bool {
        self.directory
            .members()
            .iter()
            .any(|m| m.id == member && m.role == MemberRole::Creator)
    }

    /// One message from `from`'s events connection.
    fn receive(&self, from: MemberId, event: PeerEvent) {
        let at = now();
        let forward = match event {
            PeerEvent::Presence { presence } => {
                let mut book = lock(&self.presence);
                let was_online = book.is_online(from);
                let changed = book.received(from, presence, at);
                let back = !was_online && book.is_online(from);
                drop(book);
                if back {
                    self.outbox.nudge(from);
                }
                changed.map(|presence| PeerEvent::Presence { presence })
            }
            other => {
                lock(&self.presence).heard(from, at);
                let theirs = match &other {
                    PeerEvent::QuestionUpdate { question } => question.target.owner() == from,
                    PeerEvent::RequestUpdate { request } => request.owner == from,
                    PeerEvent::Roster { .. } => self.is_creator(from),
                    PeerEvent::QuestionRemoved { .. } => true,
                    PeerEvent::Presence { .. } => false,
                };
                theirs.then_some(other)
            }
        };
        if let Some(event) = forward {
            let _ = self.events.send(event);
        }
    }

    /// The events connection to `member` closed or could not open.
    fn lost(&self, member: MemberId) {
        if let Some(presence) = lock(&self.presence).lost(member) {
            let _ = self.events.send(PeerEvent::Presence { presence });
        }
    }

    fn outcome_event(&self, outcome: OutboxOutcome) -> Option<PeerEvent> {
        let OutboxOutcome { entry, result } = outcome;
        match result {
            Ok(Delivered::Request(request)) => Some(PeerEvent::RequestUpdate { request }),
            // The owner's Mac publishes the question from here on.
            Ok(Delivered::Ask(_)) => None,
            Err(error) => match &entry.payload {
                OutboxPayload::Ask { .. } => self
                    .local_question(&entry, QuestionStatus::Failed, Some(error))
                    .ok()
                    .map(|question| PeerEvent::QuestionUpdate {
                        question: Box::new(question),
                    }),
                OutboxPayload::Request { .. } => self
                    .local_request(&entry, RequestStatus::Failed, Some(error))
                    .ok()
                    .map(|request| PeerEvent::RequestUpdate {
                        request: Box::new(request),
                    }),
            },
        }
    }

    /// The question as this Mac knows it while it never reached the owner's Mac.
    fn local_question(
        &self,
        entry: &OutboxEntry,
        status: QuestionStatus,
        error: Option<ApiError>,
    ) -> Result<Question> {
        let OutboxPayload::Ask { ask } = &entry.payload else {
            return Err(ApiError::new(ErrorCode::Internal, "not a question"));
        };
        Ok(Question {
            id: ask.question_id,
            thread_id: ask.thread_id.unwrap_or(ask.question_id),
            asker: self.me()?,
            target: ask.target,
            text: ask.text.clone(),
            status,
            candidates: Vec::new(),
            agent_id: match ask.target {
                AskTarget::Agent { agent_id, .. } => Some(agent_id),
                AskTarget::Person { .. } => None,
            },
            answer: None,
            error,
            created_at: entry.queued_at,
            updated_at: now(),
        })
    }

    /// The request as this Mac knows it while it never reached the owner's Mac.
    fn local_request(
        &self,
        entry: &OutboxEntry,
        status: RequestStatus,
        error: Option<ApiError>,
    ) -> Result<WorkRequest> {
        let OutboxPayload::Request { request } = &entry.payload else {
            return Err(ApiError::new(ErrorCode::Internal, "not a work request"));
        };
        Ok(WorkRequest {
            id: request.request_id,
            requester: self.me()?,
            requester_agent: request.via_agent.clone(),
            chain: request.chain,
            owner: entry.member,
            agent_id: request.agent_id,
            prompt: request.prompt.clone(),
            idempotency_key: request.idempotency_key.clone(),
            status,
            result: None,
            error,
            created_at: entry.queued_at,
            updated_at: now(),
        })
    }

    fn me(&self) -> Result<MemberId> {
        self.directory
            .me()
            .ok_or_else(|| ApiError::new(ErrorCode::NotMember, "this Mac is not in a team"))
    }

    async fn read_events(
        &self,
        member: MemberId,
        mut socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
        stop: &mut watch::Receiver<bool>,
    ) {
        loop {
            let frame = tokio::select! {
                frame = tokio::time::timeout(self.config.idle_timeout, socket.next()) => frame,
                _ = stopped(stop) => {
                    let _ = socket.close(None).await;
                    return;
                }
            };
            match frame {
                Ok(Some(Ok(Message::Text(text)))) => {
                    // An event this Mac does not know yet (from a newer Mac) is skipped.
                    if let Ok(event) = serde_json::from_str::<PeerEvent>(text.as_str()) {
                        self.receive(member, event);
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => return,
                // Pings (answered by tungstenite) and anything else.
                Ok(Some(Ok(_))) => lock(&self.presence).heard(member, now()),
            }
        }
    }
}

/// Keeps one events connection to `member` open, reconnecting with backoff.
async fn run_link(inner: Arc<ClientInner>, member: MemberId, nudge: Arc<Notify>) {
    let mut stop = inner.stop.subscribe();
    let mut failures = 0u32;
    loop {
        if *stop.borrow() {
            return;
        }
        if let Some(address) = inner.directory.address(member) {
            let url = format!("ws://{address}{PATH_EVENTS}");
            let connect = tokio::time::timeout(
                inner.config.connect_timeout,
                tokio_tungstenite::connect_async(url),
            );
            let connected = tokio::select! {
                connected = connect => connected,
                _ = stopped(&mut stop) => return,
            };
            if let Ok(Ok((socket, _response))) = connected {
                failures = 0;
                inner.read_events(member, socket, &mut stop).await;
            }
        }
        inner.lost(member);
        failures = failures.saturating_add(1);
        let delay = inner.config.reconnect.delay(failures);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = nudge.notified() => {}
            _ = stopped(&mut stop) => return,
        }
    }
}

/// Turns what the outbox finished into events for listeners.
async fn forward_outcomes(
    inner: Arc<ClientInner>,
    mut outcomes: broadcast::Receiver<OutboxOutcome>,
) {
    let mut stop = inner.stop.subscribe();
    loop {
        let outcome = tokio::select! {
            outcome = outcomes.recv() => match outcome {
                Ok(outcome) => outcome,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            },
            _ = stopped(&mut stop) => return,
        };
        let member = outcome.entry.member;
        if outcome.result.is_ok() && !lock(&inner.presence).is_online(member) {
            // Their Mac took a call, so it is back: reconnect without waiting.
            inner.reconnect(member);
        }
        if let Some(event) = inner.outcome_event(outcome) {
            let _ = inner.events.send(event);
        }
    }
}

const NO_BODY: Option<&()> = None;

enum CallError {
    /// Not reached (`peer_offline`); worth retrying.
    Unreachable(ApiError),
    /// The teammate's Mac answered with an error.
    Rejected(ApiError),
}

impl From<CallError> for ApiError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::Unreachable(error) | CallError::Rejected(error) => error,
        }
    }
}

struct Http {
    client: reqwest::Client,
    directory: Arc<dyn PeerDirectory>,
}

impl Http {
    fn url(&self, member: MemberId, path: &str) -> std::result::Result<String, CallError> {
        let address = self.directory.address(member).ok_or_else(|| {
            CallError::Rejected(ApiError::new(
                ErrorCode::NotFound,
                format!("no tailnet address for member {member}"),
            ))
        })?;
        Ok(format!("http://{address}{path}"))
    }

    async fn ask(
        &self,
        member: MemberId,
        ask: &PeerAsk,
    ) -> std::result::Result<PeerAskAccepted, CallError> {
        let url = self.url(member, PATH_ASK)?;
        self.call(Method::POST, url, Some(ask)).await
    }

    async fn create_request(
        &self,
        member: MemberId,
        request: &PeerRequestCreate,
    ) -> std::result::Result<WorkRequest, CallError> {
        let url = self.url(member, PATH_REQUESTS)?;
        self.call(Method::POST, url, Some(request)).await
    }

    async fn call<B, T>(
        &self,
        method: Method,
        url: String,
        body: Option<&B>,
    ) -> std::result::Result<T, CallError>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let mut request = self.client.request(method, &url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| CallError::Unreachable(offline(&url, &e)))?;
        let status = response.status();
        // A reply cut off midway may still have been acted on; retrying is safe
        // because replays reuse the asker's IDs.
        let bytes = response
            .bytes()
            .await
            .map_err(|e| CallError::Unreachable(offline(&url, &e)))?;
        if status.is_success() {
            return serde_json::from_slice(&bytes).map_err(|e| {
                CallError::Rejected(ApiError::new(
                    ErrorCode::Internal,
                    format!("unexpected reply from {url}: {e}"),
                ))
            });
        }
        match serde_json::from_slice::<PeerErrorBody>(&bytes) {
            Ok(body) if body.error.code == ErrorCode::PeerOffline => {
                Err(CallError::Unreachable(body.error))
            }
            Ok(body) => Err(CallError::Rejected(body.error)),
            // Not a Formation reply: a proxy or a Mac shutting down.
            Err(_)
                if matches!(
                    status,
                    StatusCode::BAD_GATEWAY
                        | StatusCode::SERVICE_UNAVAILABLE
                        | StatusCode::GATEWAY_TIMEOUT
                ) =>
            {
                Err(CallError::Unreachable(ApiError::new(
                    ErrorCode::PeerOffline,
                    format!("{url} answered {status}"),
                )))
            }
            Err(_) => Err(CallError::Rejected(ApiError::new(
                ErrorCode::Internal,
                format!("{url} answered {status}"),
            ))),
        }
    }
}

#[async_trait]
impl Deliver for Http {
    async fn deliver(&self, member: MemberId, payload: &OutboxPayload) -> Delivery {
        // The call was queued with an address; not resolving one now (the
        // directory could not reach Tailscale, say) is not a refusal.
        if self.directory.address(member).is_none() {
            return Delivery::Unreachable(ApiError::new(
                ErrorCode::PeerOffline,
                format!("no tailnet address for member {member} yet"),
            ));
        }
        let result = match payload {
            OutboxPayload::Ask { ask } => self.ask(member, ask).await.map(Delivered::Ask),
            OutboxPayload::Request { request } => self
                .create_request(member, request)
                .await
                .map(|created| Delivered::Request(Box::new(created))),
        };
        match result {
            Ok(delivered) => Delivery::Delivered(delivered),
            Err(CallError::Unreachable(error)) => Delivery::Unreachable(error),
            Err(CallError::Rejected(error)) if rate_limited_for_now(payload, &error) => {
                Delivery::Unreachable(error)
            }
            Err(CallError::Rejected(error)) => Delivery::Rejected(error),
        }
    }
}

/// `rate_limited` is final only for a chain deeper than
/// [`MAX_CHAIN_DEPTH`]; otherwise it is the per-minute limit, which a teammate
/// coming back to more than [`MAX_CALLS_PER_MINUTE`] waiting calls hits at once.
/// Those calls wait and are sent again with the backoff instead of failing.
///
/// [`MAX_CALLS_PER_MINUTE`]: formation_proto::policy::MAX_CALLS_PER_MINUTE
fn rate_limited_for_now(payload: &OutboxPayload, error: &ApiError) -> bool {
    let chain = match payload {
        OutboxPayload::Ask { ask } => ask.chain,
        OutboxPayload::Request { request } => request.chain,
    };
    error.code == ErrorCode::RateLimited && chain.depth <= MAX_CHAIN_DEPTH
}

fn offline(url: &str, error: &reqwest::Error) -> ApiError {
    ApiError::new(ErrorCode::PeerOffline, format!("{url}: {error}"))
}

fn not_delivered() -> ApiError {
    ApiError::new(
        ErrorCode::PeerOffline,
        "still waiting to reach the teammate's Mac",
    )
}

/// The creator's address from an invitation, with the peer port when it has
/// none: `100.64.0.1` becomes `100.64.0.1:7413`.
pub fn peer_address(address: &str) -> String {
    let address = address.trim();
    if let Ok(ip) = address.parse::<IpAddr>() {
        return SocketAddr::new(ip, PEER_PORT).to_string();
    }
    let has_port = address.parse::<SocketAddr>().is_ok()
        || address
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok());
    if has_port {
        address.to_owned()
    } else {
        format!("{address}:{PEER_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use formation_proto::common::{AgentId, ChainId};
    use formation_proto::policy::Chain;

    use super::*;

    fn queued_ask(depth: u8) -> OutboxPayload {
        OutboxPayload::Ask {
            ask: PeerAsk {
                question_id: QuestionId::generate(),
                thread_id: None,
                target: AskTarget::Agent {
                    member_id: MemberId::generate(),
                    agent_id: AgentId::generate(),
                },
                text: "Is the login API done?".into(),
                via_agent: None,
                chain: Chain {
                    id: ChainId::generate(),
                    depth,
                },
            },
        }
    }

    #[test]
    fn only_the_per_minute_limit_keeps_a_queued_call_waiting() {
        let limited = ApiError::new(ErrorCode::RateLimited, "30 per minute");
        assert!(rate_limited_for_now(&queued_ask(0), &limited));
        assert!(rate_limited_for_now(&queued_ask(MAX_CHAIN_DEPTH), &limited));
        assert!(!rate_limited_for_now(
            &queued_ask(MAX_CHAIN_DEPTH + 1),
            &limited
        ));
        let refused = ApiError::new(ErrorCode::AgentNotShared, "private");
        assert!(!rate_limited_for_now(&queued_ask(0), &refused));
    }

    #[test]
    fn invitation_addresses_get_the_peer_port() {
        assert_eq!(peer_address("100.64.0.1"), "100.64.0.1:7413");
        assert_eq!(peer_address("100.64.0.1:9000"), "100.64.0.1:9000");
        assert_eq!(
            peer_address("fd7a:115c:a1e0::1"),
            "[fd7a:115c:a1e0::1]:7413"
        );
        assert_eq!(
            peer_address("[fd7a:115c:a1e0::1]:7413"),
            "[fd7a:115c:a1e0::1]:7413"
        );
        assert_eq!(
            peer_address("macmini.tail0000.ts.net"),
            "macmini.tail0000.ts.net:7413"
        );
        assert_eq!(peer_address(" macmini:7413 "), "macmini:7413");
    }
}
