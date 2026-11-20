//! The peer server over iroh (TASK-046): the same router and [`ApiState`] as
//! the TCP server, served over the QUIC connections of an iroh endpoint, with
//! the caller identified by the key the handshake proved instead of a
//! Tailscale address. Built only with the `iroh` feature; formationd does not
//! run it yet.
//!
//! Each bidirectional stream is one HTTP/1.1 connection, served by hyper with
//! upgrades, so `WS /v1/events` works as on TCP. Every request carries the
//! caller's identity ([`whois_for`] of its key) in a request extension that
//! only this module creates; the routes read it in place of WhoIs. The
//! server's [`CallerIdentity`] refuses every address, so a request without it
//! can never be identified.
//!
//! | Rule | Checked | The caller sees |
//! | --- | --- | --- |
//! | The address proves it receives what is sent to it | each incoming connection, before any state | a stateless QUIC retry |
//! | At most `max_handshakes` handshakes and admissions in progress | each incoming connection, before spawning | a QUIC refusal |
//! | Handshake done within `handshake_timeout` | each incoming connection | the attempt dropped |
//! | ALPN `formation-peer/1` | TLS, when the endpoint comes from [`endpoint_builder`](crate::peer::iroh::endpoint_builder); then here | a failed handshake, else [`CloseCode::UnexpectedProtocol`] |
//! | The key is a member and not this Mac's own | once, before any stream: an in-memory set of the roster's identities, then the policy | [`CloseCode::NotMember`] |
//! | At most `max_connections` in all, `max_connections_per_key` per key | once, after membership | [`CloseCode::TooManyConnections`], [`CloseCode::TooManyConnectionsForKey`] |
//! | No unidirectional streams | each admitted connection | none can be opened |
//! | At most `max_streams_per_connection` streams served at once | each connection | the next stream waits, unaccepted, until one ends |
//! | Still a member | before each stream, and on every roster change or recheck | [`CloseCode::NotMember`] |
//! | Request head within `header_read_timeout` | each stream | the stream ends without an answer |
//! | Call answered within `request_timeout` | each call | `408`, and the stream ends |
//! | Member and policy checks of each call | the routes, as on TCP | `403` with the contract's error body |
//!
//! `POST /v1/team/join` answers `not_found` here: [`IrohApiDeps`] has no join
//! handler, so nobody can join a team over iroh until TASK-047 adds a way.
//!
//! The in-memory set holds the roster's (`tailscale_login`, `tailscale_node`)
//! pairs, so a stranger's completed handshake costs no store read. It is
//! rebuilt when the hub reports a roster event or a recheck; a member added
//! without one is refused until the next. The policy stays the authority: a
//! key in the set is still authenticated.
//!
//! An events socket stops counting toward `max_streams_per_connection` once it
//! is upgraded (axum runs it in its own task); it still counts toward the QUIC
//! limit of [`MAX_STREAMS_PER_CONNECTION`] open streams. It ends on roster
//! changes and on shutdown as on TCP, and when its connection closes.
//!
//! [`IrohPeerApi::shutdown`] stops accepting, gives the calls in progress
//! `shutdown_grace`, aborts what is left, and closes every connection it
//! accepted with [`CloseCode::Shutdown`]; it returns in bounded time whatever
//! a caller does. Dropping it without `shutdown` closes those connections the
//! same way and aborts its tasks at once; the events sockets axum runs on its
//! own end when their connection closes. Nothing is logged;
//! [`IrohPeerApi::stats`] counts what used to go unseen.
//!
//! Still open:
//!
//! - Joining a team over iroh (TASK-047).
//! - The client side; whose relays and address lookup production uses; where
//!   the limits should come from (settings, or the team).
//! - Capping events sockets per member, and the WebSocket message size: both
//!   live in code shared with the TCP server, whose behaviour this step does
//!   not change.
//! - A response the caller never reads holds its stream's slot until the
//!   connection ends or the server stops.
//! - The key: the file's creation race (see [`peer::iroh`](crate::peer::iroh)),
//!   keeping it in the Keychain, and a way to rotate it.
//! - Logging refusals.
//! - Building the spike module under a feature of its own.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::{Json, Router};
use hyper::body::Incoming as IncomingBody;
use hyper::service::{Service as _, service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use iroh::endpoint::{Connection, Incoming, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointId};
use tokio::io::Join;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};

use formation_proto::common::{ApiError, ErrorCode};
use formation_proto::peer::PeerErrorBody;
use formation_proto::team::WhoIs;

use super::caller::{CallerIdentity, not_member};
use super::{ApiState, AskEndpoints, PeerApiDeps, PeerEventHub, RequestEndpoints, routes};
use crate::peer::iroh::{ALPN, CloseCode, MAX_STREAMS_PER_CONNECTION, PeerKey, whois_for};
use crate::peer::outbox::{lock, stopped};
use crate::ports::{Policy, Result, TeamRepo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrohApiConfig {
    /// How often each events connection is pinged, as on TCP.
    pub heartbeat: Duration,
    /// Admitted connections at once, from all keys.
    pub max_connections: usize,
    /// Admitted connections at once from one key.
    pub max_connections_per_key: usize,
    /// Streams served at once on one connection.
    pub max_streams_per_connection: usize,
    /// Incoming connections in their handshake or admission at once; more are
    /// refused.
    pub max_handshakes: usize,
    /// How long a stream may take to send a request head.
    pub header_read_timeout: Duration,
    /// How long an incoming connection may take to finish its handshake.
    pub handshake_timeout: Duration,
    /// How long one call may take, its body included.
    pub request_timeout: Duration,
    /// How long `shutdown` lets calls in progress finish.
    pub shutdown_grace: Duration,
}

impl Default for IrohApiConfig {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(15),
            max_connections: 64,
            max_connections_per_key: 4,
            max_streams_per_connection: MAX_STREAMS_PER_CONNECTION as usize,
            max_handshakes: 64,
            header_read_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(60),
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

impl IrohApiConfig {
    /// Every limit and every timeout must be above zero.
    fn check(&self) -> io::Result<()> {
        let limits = [
            ("max_connections", self.max_connections),
            ("max_connections_per_key", self.max_connections_per_key),
            (
                "max_streams_per_connection",
                self.max_streams_per_connection,
            ),
            ("max_handshakes", self.max_handshakes),
        ];
        let timeouts = [
            ("heartbeat", self.heartbeat),
            ("header_read_timeout", self.header_read_timeout),
            ("handshake_timeout", self.handshake_timeout),
            ("request_timeout", self.request_timeout),
            ("shutdown_grace", self.shutdown_grace),
        ];
        let zero = limits
            .iter()
            .find(|(_, limit)| *limit == 0)
            .map(|(name, _)| *name)
            .or_else(|| {
                timeouts
                    .iter()
                    .find(|(_, timeout)| timeout.is_zero())
                    .map(|(name, _)| *name)
            });
        match zero {
            Some(name) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("IrohApiConfig::{name} must be above zero"),
            )),
            None => Ok(()),
        }
    }
}

/// [`PeerApiDeps`] without `identity`, since over iroh the caller is the key
/// the handshake proved, and without `join`, since nobody joins a team over
/// iroh yet (TASK-047).
pub struct IrohApiDeps {
    pub policy: Arc<dyn Policy>,
    /// This Mac's member and the roster (`GET /v1/team`).
    pub team: Arc<dyn TeamRepo + Send + Sync>,
    pub ask: Option<Arc<dyn AskEndpoints>>,
    pub requests: Option<Arc<dyn RequestEndpoints>>,
    pub hub: PeerEventHub,
}

/// Counts since the server started.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IrohApiStats {
    /// Stateless retries sent to addresses not yet validated.
    pub address_retries: u64,
    /// Refused before the handshake: every handshake slot was taken.
    pub refused_handshake_limit: u64,
    /// Handshakes that failed, timed out, or negotiated another ALPN.
    pub handshake_failures: u64,
    /// Refused before any stream: not a member, or this Mac's own member.
    pub refused_not_member: u64,
    /// Refused before any stream: over `max_connections` or
    /// `max_connections_per_key`.
    pub refused_over_limit: u64,
    pub connections_admitted: u64,
    /// Admitted connections closed because the caller stopped being a member.
    pub closed_not_member: u64,
    /// Streams handed to the HTTP server.
    pub streams_served: u64,
    /// Streams ended because no request head came within
    /// `header_read_timeout`.
    pub header_timeouts: u64,
    /// Calls answered `408` after `request_timeout`.
    pub request_timeouts: u64,
}

/// The caller of a request on this server: the identity of the key its
/// connection's handshake proved. Only this module creates one, and only as a
/// request extension, so a request cannot bring its own.
#[derive(Clone)]
pub(super) struct IrohCaller(WhoIs);

impl IrohCaller {
    pub(super) fn whois(&self) -> &WhoIs {
        &self.0
    }
}

/// A running peer server over iroh.
pub struct IrohPeerApi {
    endpoint_id: EndpointId,
    hub: PeerEventHub,
    server: Arc<Server>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl IrohPeerApi {
    /// Starts accepting on `endpoint`. Call inside the tokio runtime.
    ///
    /// Build the endpoint with
    /// [`endpoint_builder`](crate::peer::iroh::endpoint_builder): the ALPN and
    /// the QUIC limits (streams, receive windows, no datagrams, idle timeout)
    /// come from there, and the server cannot check how its endpoint was
    /// built. It does refuse a connection that negotiated another ALPN and
    /// lets no unidirectional stream be opened on what it admits. The
    /// endpoint stays the caller's, to dial teammates with too: the server
    /// never closes it, and one server at a time should accept on it.
    ///
    /// Refuses a config with a zero limit or timeout (`InvalidInput`).
    pub fn serve(endpoint: Endpoint, config: IrohApiConfig, deps: IrohApiDeps) -> io::Result<Self> {
        config.check()?;
        let (stop, stopped) = watch::channel(false);
        let hub = deps.hub.clone();
        let state = api_state(deps, config.heartbeat, stopped.clone());
        let router = TowerToHyperService::new(routes::router(state.clone()));
        let server = Arc::new(Server {
            state,
            router,
            config,
            counters: Counters::default(),
            handshakes: Arc::new(Semaphore::new(config.max_handshakes)),
            members: Mutex::new(None),
            admissions: Mutex::default(),
            stop: stopped,
        });
        // Subscribe before reading the roster, so no change is missed.
        let membership = hub.membership_changes();
        server.refresh_members();
        let endpoint_id = endpoint.id();
        let task = tokio::spawn(accept_connections(endpoint, server.clone(), membership));
        Ok(Self {
            endpoint_id,
            hub,
            server,
            stop,
            task,
        })
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint_id
    }

    pub fn hub(&self) -> &PeerEventHub {
        &self.hub
    }

    pub fn stats(&self) -> IrohApiStats {
        self.server.counters.snapshot()
    }

    /// Stops accepting, ends every events connection, gives the calls in
    /// progress `shutdown_grace`, aborts the rest, and closes the connections
    /// it accepted. The endpoint stays open.
    pub async fn shutdown(mut self) {
        self.stop.send_replace(true);
        let _ = (&mut self.task).await;
    }
}

impl Drop for IrohPeerApi {
    /// Without `shutdown`: closes the accepted connections, which ends the
    /// events sockets axum runs on its own, and aborts every task at once.
    /// After `shutdown` there is nothing left to do.
    fn drop(&mut self) {
        for (connection, _) in lock(&self.server.admissions).open.values() {
            CloseCode::Shutdown.close(connection);
        }
        self.task.abort();
    }
}

/// The server's state: the shared constructor, with an identity that refuses
/// every address and no join handler.
fn api_state(
    deps: IrohApiDeps,
    heartbeat: Duration,
    shutdown: watch::Receiver<bool>,
) -> Arc<ApiState> {
    let deps = PeerApiDeps {
        identity: Arc::new(NoAddress),
        policy: deps.policy,
        team: deps.team,
        join: None,
        ask: deps.ask,
        requests: deps.requests,
        hub: deps.hub,
    };
    ApiState::new(deps, heartbeat, shutdown)
}

/// No request on this server is identified by an address.
struct NoAddress;

#[async_trait]
impl CallerIdentity for NoAddress {
    async fn identify(&self, _addr: SocketAddr) -> Result<WhoIs> {
        Err(not_member(
            "the iroh peer server identifies callers by key only",
        ))
    }
}

struct Server {
    state: Arc<ApiState>,
    router: TowerToHyperService<Router>,
    config: IrohApiConfig,
    counters: Counters,
    handshakes: Arc<Semaphore>,
    /// The roster's (`tailscale_login`, `tailscale_node`) pairs; `None` when
    /// the roster could not be read, and then every key goes to the policy.
    members: Mutex<Option<HashSet<(String, String)>>>,
    admissions: Mutex<Admissions>,
    stop: watch::Receiver<bool>,
}

#[derive(Default)]
struct Admissions {
    per_key: HashMap<EndpointId, usize>,
    /// The admitted connections by `stable_id`, so a drop can close them.
    open: HashMap<usize, (Connection, EndpointId)>,
}

/// One admitted connection, counted against the limits until dropped.
struct Admission {
    server: Arc<Server>,
    id: usize,
}

impl Server {
    fn refresh_members(&self) {
        let members = self.state.team.members().ok().map(|members| {
            members
                .into_iter()
                .map(|member| (member.tailscale_login, member.tailscale_node))
                .collect()
        });
        *lock(&self.members) = members;
    }

    /// Whether `whois` may be a member: in the in-memory set, or the set
    /// could not be built.
    fn may_be_member(&self, whois: &WhoIs) -> bool {
        match &*lock(&self.members) {
            Some(members) => members.contains(&(whois.login.clone(), whois.node.clone())),
            None => true,
        }
    }

    async fn is_member(&self, whois: &WhoIs) -> bool {
        self.may_be_member(whois) && self.state.authenticate(whois).await.is_ok()
    }

    /// Answers an incoming connection at once: a stateless retry to an
    /// address not yet validated, a refusal when every handshake slot is
    /// taken, else the connection with its slot.
    fn take(&self, incoming: Incoming) -> Option<(Incoming, OwnedSemaphorePermit)> {
        let incoming = if incoming.remote_addr_validated() {
            incoming
        } else {
            match incoming.retry() {
                Ok(()) => {
                    count(&self.counters.address_retries);
                    return None;
                }
                Err(validated) => validated.into_incoming(),
            }
        };
        match self.handshakes.clone().try_acquire_owned() {
            Ok(slot) => Some((incoming, slot)),
            Err(_) => {
                count(&self.counters.refused_handshake_limit);
                incoming.refuse();
                None
            }
        }
    }

    fn admit(
        self: &Arc<Self>,
        connection: &Connection,
    ) -> std::result::Result<Admission, CloseCode> {
        let key = connection.remote_id();
        let mut admissions = lock(&self.admissions);
        if admissions.open.len() >= self.config.max_connections {
            return Err(CloseCode::TooManyConnections);
        }
        let held = admissions.per_key.get(&key).copied().unwrap_or(0);
        if held >= self.config.max_connections_per_key {
            return Err(CloseCode::TooManyConnectionsForKey);
        }
        let id = connection.stable_id();
        admissions.per_key.insert(key, held + 1);
        admissions.open.insert(id, (connection.clone(), key));
        Ok(Admission {
            server: self.clone(),
            id,
        })
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut admissions = lock(&self.server.admissions);
        let Some((_, key)) = admissions.open.remove(&self.id) else {
            return;
        };
        if let Some(held) = admissions.per_key.get_mut(&key) {
            *held -= 1;
            if *held == 0 {
                admissions.per_key.remove(&key);
            }
        }
    }
}

#[derive(Default)]
struct Counters {
    address_retries: AtomicU64,
    refused_handshake_limit: AtomicU64,
    handshake_failures: AtomicU64,
    refused_not_member: AtomicU64,
    refused_over_limit: AtomicU64,
    connections_admitted: AtomicU64,
    closed_not_member: AtomicU64,
    streams_served: AtomicU64,
    header_timeouts: AtomicU64,
    request_timeouts: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> IrohApiStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::SeqCst);
        IrohApiStats {
            address_retries: read(&self.address_retries),
            refused_handshake_limit: read(&self.refused_handshake_limit),
            handshake_failures: read(&self.handshake_failures),
            refused_not_member: read(&self.refused_not_member),
            refused_over_limit: read(&self.refused_over_limit),
            connections_admitted: read(&self.connections_admitted),
            closed_not_member: read(&self.closed_not_member),
            streams_served: read(&self.streams_served),
            header_timeouts: read(&self.header_timeouts),
            request_timeouts: read(&self.request_timeouts),
        }
    }
}

/// Counted before the caller can see the outcome, so a test that has seen it
/// reads the count.
fn count(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::SeqCst);
}

async fn accept_connections(
    endpoint: Endpoint,
    server: Arc<Server>,
    mut membership: watch::Receiver<u64>,
) {
    let mut stop = server.stop.clone();
    let mut connections = JoinSet::new();
    // The hub's sender lives in the server's own state, so this stays true.
    let mut hub_open = true;
    loop {
        while connections.try_join_next().is_some() {}
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                if let Some((incoming, slot)) = server.take(incoming) {
                    connections.spawn(serve_connection(incoming, server.clone(), slot));
                }
            }
            changed = membership.changed(), if hub_open => match changed {
                Ok(()) => server.refresh_members(),
                Err(_) => hub_open = false,
            },
            _ = stopped(&mut stop) => break,
        }
    }
    while connections.join_next().await.is_some() {}
}

/// Admission. Membership is checked once here, before any stream, so a
/// stranger holds no stream at all; then the connection limits. `handshake`
/// is the slot held until the decision.
///
/// Early data: iroh 1.4.1 has no setting to refuse 0-RTT on the server (its
/// TLS configuration sets `max_early_data_size`, and `ServerConfigBuilder`
/// exposes no TLS setting). It is safe here because:
///
/// - the application sees a connection only after its handshake has
///   completed, so a replayed flight, whose handshake cannot complete, is
///   never served;
/// - resumption is single-use: iroh leaves rustls' server defaults, a session
///   cache in memory and no ticketer, and rustls takes a session out of the
///   cache when it resumes it, so a replayed early-data flight finds nothing
///   and its early data is refused by TLS;
/// - our client connects with `Endpoint::connect`, which waits for the full
///   handshake and never takes the 0-RTT path.
///
/// This rests on there being no 0-RTT accept path (`Accepting::into_0rtt` is
/// never called) and no hook that acts before the handshake completes
/// (`EndpointHooks::after_handshake` runs after it, and the server installs
/// none).
async fn serve_connection(
    incoming: Incoming,
    server: Arc<Server>,
    handshake: OwnedSemaphorePermit,
) {
    let counters = &server.counters;
    let mut stop = server.stop.clone();
    // Subscribed before the admission check, so a change during it is seen.
    let mut membership = server.state.hub.membership_changes();
    let finished = tokio::time::timeout(server.config.handshake_timeout, incoming);
    let connection = tokio::select! {
        finished = finished => match finished {
            Ok(Ok(connection)) => connection,
            _ => {
                count(&counters.handshake_failures);
                return;
            }
        },
        _ = stopped(&mut stop) => return,
    };
    if connection.alpn() != ALPN {
        count(&counters.handshake_failures);
        CloseCode::UnexpectedProtocol.close(&connection);
        return;
    }
    let whois = whois_for(PeerKey(connection.remote_id()));
    let member = tokio::select! {
        member = server.is_member(&whois) => member,
        _ = stopped(&mut stop) => {
            CloseCode::Shutdown.close(&connection);
            return;
        }
    };
    if !member {
        count(&counters.refused_not_member);
        CloseCode::NotMember.close(&connection);
        return;
    }
    let admission = match server.admit(&connection) {
        Ok(admission) => admission,
        Err(limit) => {
            count(&counters.refused_over_limit);
            limit.close(&connection);
            return;
        }
    };
    drop(handshake);
    // The endpoint should already allow none; this holds whatever it allows.
    connection.set_max_concurrent_uni_streams(VarInt::from_u32(0));
    count(&counters.connections_admitted);
    let caller = IrohCaller(whois);
    serve_streams(&connection, caller, &server, &mut stop, &mut membership).await;
    drop(admission);
}

/// Serves the connection's streams until it closes, the server stops, or the
/// caller stops being a member, checked before each stream and on every
/// roster change or recheck.
///
/// A stream over the per-connection limit is not accepted until a served one
/// ends: it waits in QUIC, and the client's request waits with it, while the
/// QUIC limit on open streams bounds how many can wait.
async fn serve_streams(
    connection: &Connection,
    caller: IrohCaller,
    server: &Arc<Server>,
    stop: &mut watch::Receiver<bool>,
    membership: &mut watch::Receiver<u64>,
) {
    let slots = Arc::new(Semaphore::new(server.config.max_streams_per_connection));
    let mut streams = JoinSet::new();
    let mut hub_open = true;
    let ending = loop {
        while streams.try_join_next().is_some() {}
        tokio::select! {
            _ = stopped(stop) => break Ending::Shutdown,
            _ = connection.closed() => break Ending::Gone,
            changed = membership.changed(), if hub_open => {
                if changed.is_err() {
                    hub_open = false;
                } else if server.state.authenticate(caller.whois()).await.is_err() {
                    break Ending::NotMember;
                }
            }
            next = next_stream(&slots, connection) => {
                let Some((slot, send, recv)) = next else {
                    break Ending::Gone;
                };
                if server.state.authenticate(caller.whois()).await.is_err() {
                    break Ending::NotMember;
                }
                count(&server.counters.streams_served);
                let stream = tokio::io::join(recv, send);
                streams.spawn(serve_stream(stream, caller.clone(), server.clone(), slot));
            }
        }
    };
    match ending {
        Ending::Shutdown => {
            // Each stream task saw the stop too and lets its call finish.
            let drained = async { while streams.join_next().await.is_some() {} };
            if tokio::time::timeout(server.config.shutdown_grace, drained)
                .await
                .is_err()
            {
                streams.shutdown().await;
            }
            CloseCode::Shutdown.close(connection);
        }
        Ending::NotMember => {
            count(&server.counters.closed_not_member);
            CloseCode::NotMember.close(connection);
            streams.shutdown().await;
        }
        Ending::Gone => streams.shutdown().await,
    }
}

enum Ending {
    Shutdown,
    NotMember,
    Gone,
}

/// A free slot, then the next stream. Dropping it midway loses neither.
async fn next_stream(
    slots: &Arc<Semaphore>,
    connection: &Connection,
) -> Option<(OwnedSemaphorePermit, SendStream, RecvStream)> {
    let slot = slots.clone().acquire_owned().await.ok()?;
    let (send, recv) = connection.accept_bi().await.ok()?;
    Some((slot, send, recv))
}

/// One HTTP/1.1 connection on one stream. `_slot` frees the connection's slot
/// when the stream ends, or when an events socket takes it over.
async fn serve_stream(
    stream: Join<RecvStream, SendStream>,
    caller: IrohCaller,
    server: Arc<Server>,
    _slot: OwnedSemaphorePermit,
) {
    let router = server.router.clone();
    let limit = server.config.request_timeout;
    let calls = server.clone();
    let service = service_fn(move |mut request: Request<IncomingBody>| {
        request.extensions_mut().insert(caller.clone());
        let call = router.call(request);
        let server = calls.clone();
        async move {
            match tokio::time::timeout(limit, call).await {
                Ok(answer) => answer,
                Err(_) => {
                    count(&server.counters.request_timeouts);
                    Ok(timed_out(limit))
                }
            }
        }
    });
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(server.config.header_read_timeout);
    let mut connection = pin!(
        http.serve_connection(TokioIo::new(stream), service)
            .with_upgrades()
    );
    let mut stop = server.stop.clone();
    let mut stopping = false;
    loop {
        tokio::select! {
            finished = connection.as_mut() => {
                if finished.is_err_and(|error| error.is_timeout()) {
                    count(&server.counters.header_timeouts);
                }
                return;
            }
            _ = stopped(&mut stop), if !stopping => {
                stopping = true;
                connection.as_mut().graceful_shutdown();
            }
        }
    }
}

/// `408` with the contract's error body, and `Connection: close` so the
/// stream ends after it. The code is `peer_offline`: the call may not have
/// run, and a repeat with the same IDs is acted on once.
fn timed_out(limit: Duration) -> Response<Body> {
    let error = ApiError::new(
        ErrorCode::PeerOffline,
        format!("the call did not finish within {limit:?}; send it again"),
    );
    let mut response = (StatusCode::REQUEST_TIMEOUT, Json(PeerErrorBody { error })).into_response();
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

#[cfg(test)]
mod tests;
