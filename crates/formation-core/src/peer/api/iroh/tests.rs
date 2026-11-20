//! The peer server over iroh, on loopback with no relay and no address lookup:
//! admission, limits, revocation, timeouts and shutdown.

use std::future::Future;
use std::net::Ipv4Addr;
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::Method;
use futures_util::StreamExt;
use iroh::SecretKey;
use iroh::endpoint::{ApplicationClose, ConnectError, ConnectingError, ConnectionError};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use formation_proto::ask::Question;
use formation_proto::common::{AgentId, MemberId, QuestionId};
use formation_proto::peer::{
    AgentsResponse, PATH_AGENTS, PATH_ASK, PATH_EVENTS, PATH_TEAM, PATH_TEAM_JOIN, PeerAsk,
    PeerAskAccepted, PeerEvent, PeerJoin, PeerQuestionAction, RosterResponse, SharedAgentView,
};
use formation_proto::policy::{Caller, PolicyVerdict, ToolCall, Visibility};
use formation_proto::team::{Member, MemberRole};

use super::super::routes::Identified;
use super::*;
use crate::peer::iroh::{Discovery, endpoint_builder};
use crate::peer::iroh_spike::{request, websocket};
use crate::peer::loopback_test::{Roster, TestAsk, TestPolicy, agent, member, peer_ask};
use crate::peer::outbox::now;
use crate::policy::TeamPolicy;
use crate::ports::ToolContext;
use crate::testkit::MemoryStore;
use crate::testkit::collab::{MemoryCollabStore, samples};

const WAIT: Duration = Duration::from_secs(20);

type Reply = io::Result<(StatusCode, Bytes)>;

async fn within<F: Future>(step: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("{step} did not finish within {WAIT:?}"))
}

/// An endpoint on loopback only, with no relay and no address lookup.
async fn endpoint() -> Endpoint {
    bind(endpoint_builder(SecretKey::generate(), Discovery::None)).await
}

async fn bind(builder: iroh::endpoint::Builder) -> Endpoint {
    let builder = builder
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("loopback address");
    within("bind", builder.bind()).await.expect("bind")
}

/// A member whose identity is `endpoint`'s key.
fn member_of(endpoint: &Endpoint, login: &str) -> Member {
    let whois = whois_for(PeerKey(endpoint.id()));
    Member {
        tailscale_login: whois.login,
        tailscale_node: whois.node,
        ..member(login, MemberRole::Member)
    }
}

/// This Mac: its endpoint, roster, doubles, and server. Change the doubles
/// before [`Mac::serve`]; the server reads the roster when it starts.
struct Mac {
    endpoint: Endpoint,
    me: Member,
    roster: Arc<Roster>,
    policy: Arc<TestPolicy>,
    /// What the server authenticates with; `policy` unless a test swaps it.
    authority: Arc<dyn Policy>,
    asked: Arc<TestAsk>,
    ask: Arc<dyn AskEndpoints>,
    hub: PeerEventHub,
    api: Option<IrohPeerApi>,
}

impl Mac {
    async fn start(config: IrohApiConfig, teammates: &[&Member]) -> Self {
        let mut mac = Self::new(teammates).await;
        mac.serve(config);
        mac
    }

    async fn new(teammates: &[&Member]) -> Self {
        Self::on(endpoint().await, teammates)
    }

    fn on(endpoint: Endpoint, teammates: &[&Member]) -> Self {
        let me = member_of(&endpoint, "dohyeon");
        let mut members = vec![me.clone()];
        members.extend(teammates.iter().map(|member| (*member).clone()));
        let roster = Arc::new(Roster {
            me: me.id,
            members: Mutex::new(members),
            addresses: Mutex::new(HashMap::new()),
        });
        let policy = Arc::new(TestPolicy {
            roster: roster.clone(),
            shared: agent(me.id, "api"),
            private: AgentId::generate(),
        });
        let hub = PeerEventHub::new();
        let asked = Arc::new(TestAsk {
            hub: hub.clone(),
            questions: Mutex::new(HashMap::new()),
            calls: Mutex::new(0),
        });
        Self {
            endpoint,
            me,
            roster,
            authority: policy.clone(),
            policy,
            ask: asked.clone(),
            asked,
            hub,
            api: None,
        }
    }

    fn deps(&self) -> IrohApiDeps {
        IrohApiDeps {
            policy: self.authority.clone(),
            team: self.roster.clone(),
            ask: Some(self.ask.clone()),
            requests: None,
            hub: self.hub.clone(),
        }
    }

    fn serve(&mut self, config: IrohApiConfig) {
        let api = IrohPeerApi::serve(self.endpoint.clone(), config, self.deps()).expect("serve");
        self.api = Some(api);
    }

    fn stats(&self) -> IrohApiStats {
        self.api.as_ref().expect("serving").stats()
    }

    async fn connect(&self, client: &Endpoint) -> Connection {
        within("connect", client.connect(self.endpoint.addr(), ALPN))
            .await
            .expect("connect")
    }

    /// Removes `member` and tells the hub, as the team module does.
    fn remove(&self, member: &Member) {
        self.roster.remove_member(member.id, now()).expect("remove");
        self.hub.publish_all(PeerEvent::Roster {
            members: TeamRepo::members(self.roster.as_ref()).expect("members"),
            version: 0,
        });
    }

    /// Shuts the server down and closes every endpoint.
    async fn stop(mut self, clients: impl IntoIterator<Item = Endpoint>) {
        if let Some(api) = self.api.take() {
            within("shutdown", api.shutdown()).await;
        }
        for client in clients {
            within("client close", client.close()).await;
        }
        within("close", self.endpoint.close()).await;
    }
}

/// How the server closed a connection `client` opened to it, at once or
/// right after the handshake.
async fn refusal(client: &Endpoint, server: &Endpoint) -> ApplicationClose {
    let error = match within("connect", client.connect(server.addr(), ALPN)).await {
        Ok(connection) => within("the close", connection.closed()).await,
        Err(error) => connect_error(error),
    };
    application_close(error)
}

fn connect_error(error: ConnectError) -> ConnectionError {
    match error {
        ConnectError::Connection { source, .. } => source,
        ConnectError::Connecting {
            source: ConnectingError::ConnectionError { source, .. },
            ..
        } => source,
        other => panic!("the connection failed otherwise: {other}"),
    }
}

fn application_close(error: ConnectionError) -> ApplicationClose {
    match error {
        ConnectionError::ApplicationClosed(close) => close,
        other => panic!("expected the server to close the connection, got {other}"),
    }
}

async fn closed_with(connection: &Connection) -> ApplicationClose {
    application_close(within("the connection's close", connection.closed()).await)
}

fn assert_closed_with(close: &ApplicationClose, code: CloseCode) {
    assert_eq!(close.error_code, VarInt::from_u32(code.code()), "{close}");
    assert_eq!(close.reason.as_ref(), code.reason().as_bytes(), "{close}");
}

async fn agents(connection: &Connection) -> Reply {
    let call = request(connection, Method::GET, PATH_AGENTS, None);
    within("GET /v1/agents", call).await
}

async fn ask(connection: &Connection, ask: &PeerAsk) -> Reply {
    let body = serde_json::to_value(ask).expect("JSON");
    let call = request(connection, Method::POST, PATH_ASK, Some(&body));
    within("POST /v1/ask", call).await
}

fn status(reply: Reply) -> StatusCode {
    reply.expect("an answer").0
}

fn error_code(body: &[u8]) -> ErrorCode {
    let body: PeerErrorBody = serde_json::from_slice(body).expect("an error body");
    body.error.code
}

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// Takes every question, tells the test, and answers only when released.
struct HeldAsk {
    arrived: mpsc::UnboundedSender<QuestionId>,
    release: Semaphore,
}

impl HeldAsk {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<QuestionId>) {
        let (arrived, arrivals) = mpsc::unbounded_channel();
        let held = Arc::new(Self {
            arrived,
            release: Semaphore::new(0),
        });
        (held, arrivals)
    }
}

#[async_trait]
impl AskEndpoints for HeldAsk {
    async fn ask(&self, _caller: Caller, ask: PeerAsk) -> Result<PeerAskAccepted> {
        let _ = self.arrived.send(ask.question_id);
        self.release.acquire().await.expect("open").forget();
        Ok(PeerAskAccepted {
            question_id: ask.question_id,
        })
    }

    async fn question_action(
        &self,
        _member: MemberId,
        _question_id: QuestionId,
        _action: PeerQuestionAction,
    ) -> Result<Question> {
        Err(ApiError::new(ErrorCode::NotFound, "not used here"))
    }
}

/// [`TestPolicy`], whose `authenticate` tells the test it was entered and
/// waits until the gate opens; once open it stays open.
struct GatedPolicy {
    inner: Arc<TestPolicy>,
    entered: mpsc::UnboundedSender<()>,
    gate: Semaphore,
}

#[async_trait]
impl Policy for GatedPolicy {
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId> {
        let _ = self.entered.send(());
        drop(self.gate.acquire().await.expect("open"));
        self.inner.authenticate(whois).await
    }
    async fn visible_agents(&self, caller: &Caller) -> Result<Vec<SharedAgentView>> {
        self.inner.visible_agents(caller).await
    }
    async fn check_ask(
        &self,
        caller: &Caller,
        target: &formation_proto::ask::AskTarget,
    ) -> Result<()> {
        self.inner.check_ask(caller, target).await
    }
    async fn check_request(&self, caller: &Caller, agent_id: AgentId) -> Result<()> {
        self.inner.check_request(caller, agent_id).await
    }
    async fn visibility(&self, agent_id: AgentId) -> Result<Visibility> {
        self.inner.visibility(agent_id).await
    }
    async fn evaluate(&self, context: &ToolContext, call: &ToolCall) -> Result<PolicyVerdict> {
        self.inner.evaluate(context, call).await
    }
}

// ---------------------------------------------------------------------------
// Calls from members
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_member_lists_the_agents_the_policy_gives_them() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;

    let (status, body) = agents(&connection).await.expect("an answer");

    assert_eq!(status, StatusCode::OK);
    let agents: AgentsResponse = serde_json::from_slice(&body).expect("agents");
    assert_eq!(agents.agents, vec![mac.policy.shared.clone()]);
    let stats = mac.stats();
    assert_eq!((stats.connections_admitted, stats.streams_served), (1, 1));
    assert!(stats.address_retries >= 1, "the first address was retried");
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_members_question_reaches_the_handler_as_that_member() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    let question = peer_ask(mac.me.id, mac.policy.shared.agent_id);

    let (status, body) = ask(&connection, &question).await.expect("an answer");

    assert_eq!(status, StatusCode::ACCEPTED);
    let accepted: PeerAskAccepted = serde_json::from_slice(&body).expect("accepted");
    assert_eq!(accepted.question_id, question.question_id);
    let asked = lock(&mac.asked.questions)
        .get(&question.question_id)
        .cloned()
        .expect("the handler saw the question");
    assert_eq!(asked.asker, minsu.id, "Caller::Member with minsu's ID");
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_members_at_once_each_reach_the_handler_as_themselves() {
    let (a, b) = (endpoint().await, endpoint().await);
    let (minsu, jisoo) = (member_of(&a, "minsu"), member_of(&b, "jisoo"));
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu, &jisoo]).await;
    let (to_a, to_b) = tokio::join!(mac.connect(&a), mac.connect(&b));

    let mut sent = Vec::new();
    for _ in 0..3 {
        let from_a = peer_ask(mac.me.id, mac.policy.shared.agent_id);
        let from_b = peer_ask(mac.me.id, mac.policy.shared.agent_id);
        let (a_reply, b_reply) = tokio::join!(ask(&to_a, &from_a), ask(&to_b, &from_b));
        assert_eq!(status(a_reply), StatusCode::ACCEPTED);
        assert_eq!(status(b_reply), StatusCode::ACCEPTED);
        sent.push((from_a.question_id, minsu.id));
        sent.push((from_b.question_id, jisoo.id));
    }

    let questions = lock(&mac.asked.questions).clone();
    for (question_id, asker) in sent {
        assert_eq!(questions[&question_id].asker, asker);
    }
    mac.stop([a, b]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_events_socket_opens_with_the_presence_hello() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;

    let mut socket = within("WS /v1/events", websocket(&connection, PATH_EVENTS))
        .await
        .expect("upgrade");
    let first = within("the hello", socket.next()).await;

    let Some(Ok(Message::Text(text))) = first else {
        panic!("expected a text message, got {first:?}");
    };
    let event: PeerEvent = serde_json::from_str(text.as_str()).expect("an event");
    let PeerEvent::Presence { presence } = event else {
        panic!("expected the presence hello, got {event:?}");
    };
    assert_eq!(presence.member_id, mac.me.id);
    assert!(presence.online);
    assert_eq!(presence.agents, vec![mac.policy.shared.clone()]);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn joining_over_iroh_answers_not_found() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    let join = serde_json::to_value(PeerJoin {
        code: "fmn1-ok".into(),
        github_login: "jisoo".into(),
        display_name: "Jisoo".into(),
    })
    .expect("JSON");

    let call = request(&connection, Method::POST, PATH_TEAM_JOIN, Some(&join));
    let (status, body) = within("POST /v1/team/join", call).await.expect("an answer");

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), ErrorCode::NotFound);
    mac.stop([client]).await;
}

/// The real policy and the in-memory collaboration store from the testkit: a
/// member recorded with the `iroh:` identity in both Tailscale fields is
/// served, and a key the store does not hold is refused.
#[tokio::test(flavor = "multi_thread")]
async fn the_team_policy_serves_a_member_recorded_with_the_iroh_identity() {
    let server = endpoint().await;
    let client = endpoint().await;
    let stranger = endpoint().await;
    let collab = Arc::new(MemoryCollabStore::new());
    let mut me = samples::member("dohyeon", MemberRole::Creator);
    let whois = whois_for(PeerKey(server.id()));
    (me.tailscale_login, me.tailscale_node) = (whois.login, whois.node);
    let mut minsu = samples::member("minsu", MemberRole::Member);
    let whois = whois_for(PeerKey(client.id()));
    (minsu.tailscale_login, minsu.tailscale_node) = (whois.login, whois.node);
    collab.put_member(&me).expect("put");
    collab.put_member(&minsu).expect("put");
    collab.set_me(me.id).expect("me");
    let deps = IrohApiDeps {
        policy: Arc::new(TeamPolicy::new(
            collab.clone(),
            Arc::new(MemoryStore::default()),
        )),
        team: collab.clone(),
        ask: None,
        requests: None,
        hub: PeerEventHub::new(),
    };
    let api = IrohPeerApi::serve(server.clone(), IrohApiConfig::default(), deps).expect("serve");

    let connection = within("connect", client.connect(server.addr(), ALPN))
        .await
        .expect("connect");
    let call = request(&connection, Method::GET, PATH_TEAM, None);
    let (status, body) = within("GET /v1/team", call).await.expect("an answer");
    assert_eq!(status, StatusCode::OK);
    let roster: RosterResponse = serde_json::from_slice(&body).expect("roster");
    assert_eq!(roster.members.len(), 2);

    let close = refusal(&stranger, &server).await;
    assert_closed_with(&close, CloseCode::NotMember);

    within("shutdown", api.shutdown()).await;
    for endpoint in [client, stranger, server] {
        within("close", endpoint.close()).await;
    }
}

// ---------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_is_closed_out_before_any_stream() {
    let stranger = endpoint().await;
    let mac = Mac::start(IrohApiConfig::default(), &[]).await;

    let close = refusal(&stranger, &mac.endpoint).await;

    assert_closed_with(&close, CloseCode::NotMember);
    let stats = mac.stats();
    assert_eq!(stats.refused_not_member, 1);
    assert_eq!(stats.connections_admitted, 0);
    assert_eq!(stats.streams_served, 0);
    mac.stop([stranger]).await;
}

/// The policy here blocks every call until its gate opens, which it never
/// does: the stranger is refused by the in-memory roster set alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_is_refused_without_a_policy_call() {
    let stranger = endpoint().await;
    let mut mac = Mac::new(&[]).await;
    let (entered, mut entries) = mpsc::unbounded_channel();
    mac.authority = Arc::new(GatedPolicy {
        inner: mac.policy.clone(),
        entered,
        gate: Semaphore::new(0),
    });
    mac.serve(IrohApiConfig::default());

    let close = refusal(&stranger, &mac.endpoint).await;

    assert_closed_with(&close, CloseCode::NotMember);
    assert!(entries.try_recv().is_err(), "the policy was asked");
    mac.stop([stranger]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn this_macs_own_member_is_refused_at_admission() {
    let client = endpoint().await;
    let mut mac = Mac::new(&[]).await;
    let whois = whois_for(PeerKey(client.id()));
    let own = Member {
        tailscale_login: whois.login,
        tailscale_node: whois.node,
        ..mac.me.clone()
    };
    mac.roster.put_member(&own).expect("put");
    mac.serve(IrohApiConfig::default());

    let close = refusal(&client, &mac.endpoint).await;

    assert_closed_with(&close, CloseCode::NotMember);
    assert_eq!(mac.stats().streams_served, 0);
    mac.stop([client]).await;
}

/// With [`endpoint_builder`] the server offers only [`ALPN`], so TLS refuses
/// another ALPN before the server sees the connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_with_another_alpn_fails_and_the_server_keeps_accepting() {
    let stranger = endpoint().await;
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;

    let other = within(
        "connect with another ALPN",
        stranger.connect(mac.endpoint.addr(), b"another-protocol/0"),
    )
    .await;
    assert!(other.is_err(), "a connection with another ALPN succeeded");

    let connection = mac.connect(&client).await;
    assert_eq!(status(agents(&connection).await), StatusCode::OK);
    mac.stop([stranger, client]).await;
}

/// An endpoint that offers another ALPN as well, against `serve`'s advice:
/// the server closes such a connection itself.
#[tokio::test(flavor = "multi_thread")]
async fn the_server_closes_a_connection_that_negotiated_another_alpn() {
    let other_alpn = b"another-protocol/0";
    let builder = endpoint_builder(SecretKey::generate(), Discovery::None)
        .alpns(vec![ALPN.to_vec(), other_alpn.to_vec()]);
    let mut mac = Mac::on(bind(builder).await, &[]);
    mac.serve(IrohApiConfig::default());
    let client = endpoint().await;

    let error = match within("connect", client.connect(mac.endpoint.addr(), other_alpn)).await {
        Ok(connection) => within("the close", connection.closed()).await,
        Err(error) => connect_error(error),
    };

    assert_closed_with(&application_close(error), CloseCode::UnexpectedProtocol);
    assert_eq!(mac.stats().handshake_failures, 1);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn handshakes_over_their_limit_are_refused() {
    let held = endpoint().await;
    let late = endpoint().await;
    let (minsu, jisoo) = (member_of(&held, "minsu"), member_of(&late, "jisoo"));
    let mut mac = Mac::new(&[&minsu, &jisoo]).await;
    let (entered, mut entries) = mpsc::unbounded_channel();
    let gated = Arc::new(GatedPolicy {
        inner: mac.policy.clone(),
        entered,
        gate: Semaphore::new(0),
    });
    mac.authority = gated.clone();
    mac.serve(IrohApiConfig {
        max_handshakes: 1,
        ..IrohApiConfig::default()
    });

    // The first connection holds the only slot while its admission waits.
    let first = mac.connect(&held).await;
    within("the first admission", entries.recv()).await;
    let refused = within("connect", late.connect(mac.endpoint.addr(), ALPN)).await;
    let error = match refused {
        Ok(connection) => within("the refusal", connection.closed()).await,
        Err(error) => connect_error(error),
    };
    assert!(
        matches!(error, ConnectionError::ConnectionClosed(_)),
        "expected a QUIC refusal, got {error}"
    );
    assert_eq!(mac.stats().refused_handshake_limit, 1);

    gated.gate.add_permits(1);
    assert_eq!(status(agents(&first).await), StatusCode::OK);
    let second = mac.connect(&late).await;
    assert_eq!(status(agents(&second).await), StatusCode::OK);
    mac.stop([held, late]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_connection_from_one_key_over_its_limit_is_closed() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let config = IrohApiConfig {
        max_connections_per_key: 1,
        ..IrohApiConfig::default()
    };
    let mac = Mac::start(config, &[&minsu]).await;
    let first = mac.connect(&client).await;
    assert_eq!(status(agents(&first).await), StatusCode::OK);

    let close = refusal(&client, &mac.endpoint).await;

    assert_closed_with(&close, CloseCode::TooManyConnectionsForKey);
    assert_eq!(status(agents(&first).await), StatusCode::OK);
    assert_eq!(mac.stats().refused_over_limit, 1);
    mac.stop([client]).await;
}

/// The first connection's slot comes back once it closes. The server learns of
/// the close on its own time, so attempts in between may still be refused;
/// the test retries until one is admitted, and would time out if the slot
/// never came back.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_connection_gives_its_slot_back() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let config = IrohApiConfig {
        max_connections_per_key: 1,
        ..IrohApiConfig::default()
    };
    let mac = Mac::start(config, &[&minsu]).await;
    let first = mac.connect(&client).await;
    assert_eq!(status(agents(&first).await), StatusCode::OK);

    first.close(VarInt::from_u32(0), b"done");
    let answered = within("a second admission", async {
        loop {
            let Ok(connection) = client.connect(mac.endpoint.addr(), ALPN).await else {
                continue;
            };
            let call = request(&connection, Method::GET, PATH_AGENTS, None);
            if let Ok((status, _)) = call.await {
                break status;
            }
        }
    })
    .await;

    assert_eq!(answered, StatusCode::OK);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_over_the_servers_limit_is_closed() {
    let (a, b) = (endpoint().await, endpoint().await);
    let (minsu, jisoo) = (member_of(&a, "minsu"), member_of(&b, "jisoo"));
    let config = IrohApiConfig {
        max_connections: 1,
        ..IrohApiConfig::default()
    };
    let mac = Mac::start(config, &[&minsu, &jisoo]).await;
    let first = mac.connect(&a).await;
    assert_eq!(status(agents(&first).await), StatusCode::OK);

    let close = refusal(&b, &mac.endpoint).await;

    assert_closed_with(&close, CloseCode::TooManyConnections);
    assert_eq!(status(agents(&first).await), StatusCode::OK);
    mac.stop([a, b]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_config_with_a_zero_limit_or_timeout_is_refused() {
    let mac = Mac::new(&[]).await;
    for (config, field) in [
        (
            IrohApiConfig {
                max_connections: 0,
                ..IrohApiConfig::default()
            },
            "max_connections",
        ),
        (
            IrohApiConfig {
                request_timeout: Duration::ZERO,
                ..IrohApiConfig::default()
            },
            "request_timeout",
        ),
    ] {
        let Err(error) = IrohPeerApi::serve(mac.endpoint.clone(), config, mac.deps()) else {
            panic!("a zero {field} was accepted");
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains(field), "{error}");
    }
    mac.stop([]).await;
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_members_idle_connection_is_closed_on_the_roster_change() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    assert_eq!(status(agents(&connection).await), StatusCode::OK);

    mac.remove(&minsu);

    assert_closed_with(&closed_with(&connection).await, CloseCode::NotMember);
    assert_eq!(mac.stats().closed_not_member, 1);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_member_loses_the_events_socket_and_the_connection() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    let mut socket = within("WS /v1/events", websocket(&connection, PATH_EVENTS))
        .await
        .expect("upgrade");
    within("the hello", socket.next()).await;

    mac.remove(&minsu);

    // The socket ends with its close frame or with its connection, whichever
    // reaches the client first.
    loop {
        match within("the socket's end", socket.next()).await {
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            Some(Ok(_)) => {}
        }
    }
    assert_closed_with(&closed_with(&connection).await, CloseCode::NotMember);
    assert!(agents(&connection).await.is_err(), "a later call failed");
    mac.stop([client]).await;
}

// ---------------------------------------------------------------------------
// Streams and calls
// ---------------------------------------------------------------------------

/// No API tells when a stream the peer opened has reached the server before
/// the server accepts it, so the second request is given a round trip on
/// another connection to arrive; a server that took it would have its handler
/// entered by then. Lifting the limit makes this test fail.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_over_the_connections_limit_waits_for_a_free_slot() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let (held, mut arrivals) = HeldAsk::new();
    let mut mac = Mac::new(&[&minsu]).await;
    mac.ask = held.clone();
    mac.serve(IrohApiConfig {
        max_streams_per_connection: 1,
        ..IrohApiConfig::default()
    });
    let connection = mac.connect(&client).await;
    let first = peer_ask(mac.me.id, mac.policy.shared.agent_id);
    let second = peer_ask(mac.me.id, mac.policy.shared.agent_id);

    let first_call = tokio::spawn({
        let (connection, first) = (connection.clone(), first.clone());
        async move { ask(&connection, &first).await }
    });
    let reached = within("the first question", arrivals.recv()).await;
    assert_eq!(reached, Some(first.question_id));
    let second_call = tokio::spawn({
        let (connection, second) = (connection.clone(), second.clone());
        async move { ask(&connection, &second).await }
    });
    let other = mac.connect(&client).await;
    assert_eq!(status(agents(&other).await), StatusCode::OK);
    assert!(arrivals.try_recv().is_err(), "the second question got in");
    assert_eq!(
        mac.stats().streams_served,
        2,
        "the first call and the probe"
    );

    held.release.add_permits(1);
    let answered = within("the first call", first_call).await.expect("task");
    assert_eq!(status(answered), StatusCode::ACCEPTED);
    let reached = within("the second question", arrivals.recv()).await;
    assert_eq!(reached, Some(second.question_id));
    held.release.add_permits(1);
    let answered = within("the second call", second_call).await.expect("task");
    assert_eq!(status(answered), StatusCode::ACCEPTED);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_without_a_request_head_ends_after_the_timeout() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let timeout = Duration::from_millis(500);
    let config = IrohApiConfig {
        header_read_timeout: timeout,
        ..IrohApiConfig::default()
    };
    let mac = Mac::start(config, &[&minsu]).await;
    let connection = mac.connect(&client).await;

    // QUIC shows the server a stream only once data arrives, so this one
    // sends the start of a head and stops.
    let (mut send, mut recv) = within("open", connection.open_bi()).await.expect("open");
    let started = Instant::now();
    within("write", send.write_all(b"GET /v1/agents HTTP/1.1\r\n"))
        .await
        .expect("write");
    assert_eq!(status(agents(&connection).await), StatusCode::OK);
    let rest = within("the stalled stream's end", recv.read_to_end(1024))
        .await
        .expect("a clean end");

    assert!(rest.is_empty(), "no answer");
    assert!(started.elapsed() >= timeout);
    assert_eq!(mac.stats().header_timeouts, 1);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_whose_body_never_comes_gets_408_and_shutdown_stays_short() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let limit = Duration::from_millis(500);
    let grace = Duration::from_secs(2);
    let mut mac = Mac::start(
        IrohApiConfig {
            request_timeout: limit,
            shutdown_grace: grace,
            ..IrohApiConfig::default()
        },
        &[&minsu],
    )
    .await;
    let connection = mac.connect(&client).await;

    let (mut send, mut recv) = within("open", connection.open_bi()).await.expect("open");
    let started = Instant::now();
    let head = "POST /v1/ask HTTP/1.1\r\nhost: formation-peer\r\n\
                content-type: application/json\r\ncontent-length: 100\r\n\r\n";
    within("write", send.write_all(head.as_bytes()))
        .await
        .expect("write");
    let answer = within("the answer", recv.read_to_end(64 * 1024))
        .await
        .expect("an answer, then the end");

    assert!(started.elapsed() >= limit);
    let answer = String::from_utf8_lossy(&answer);
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(answer.contains("peer_offline"), "{answer}");
    assert_eq!(mac.stats().request_timeouts, 1);
    let api = mac.api.take().expect("serving");
    tokio::time::timeout(grace, api.shutdown())
        .await
        .expect("shutdown within the grace period");
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_with_a_call_stuck_in_a_handler_returns_after_the_grace_period() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let (held, mut arrivals) = HeldAsk::new();
    let grace = Duration::from_millis(300);
    let mut mac = Mac::new(&[&minsu]).await;
    mac.ask = held;
    mac.serve(IrohApiConfig {
        shutdown_grace: grace,
        ..IrohApiConfig::default()
    });
    let connection = mac.connect(&client).await;
    let question = peer_ask(mac.me.id, mac.policy.shared.agent_id);
    let stuck = tokio::spawn({
        let connection = connection.clone();
        async move { ask(&connection, &question).await }
    });
    within("the question", arrivals.recv()).await;

    let started = Instant::now();
    let api = mac.api.take().expect("serving");
    within("shutdown", api.shutdown()).await;

    assert!(started.elapsed() >= grace);
    assert_closed_with(&closed_with(&connection).await, CloseCode::Shutdown);
    let call = within("the stuck call", stuck).await.expect("task");
    assert!(call.is_err(), "the stuck call failed");
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_closes_the_connections_and_leaves_the_endpoint_open() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mut mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    assert_eq!(status(agents(&connection).await), StatusCode::OK);

    let api = mac.api.take().expect("serving");
    within("shutdown", api.shutdown()).await;

    assert_closed_with(&closed_with(&connection).await, CloseCode::Shutdown);
    assert!(!mac.endpoint.is_closed());
    mac.serve(IrohApiConfig::default());
    let again = mac.connect(&client).await;
    assert_eq!(status(agents(&again).await), StatusCode::OK);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_server_closes_its_connections() {
    let client = endpoint().await;
    let minsu = member_of(&client, "minsu");
    let mut mac = Mac::start(IrohApiConfig::default(), &[&minsu]).await;
    let connection = mac.connect(&client).await;
    assert_eq!(status(agents(&connection).await), StatusCode::OK);

    drop(mac.api.take());

    assert_closed_with(&closed_with(&connection).await, CloseCode::Shutdown);
    mac.stop([client]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_the_iroh_caller_is_not_identified_by_its_address() {
    let mac = Mac::new(&[]).await;
    let (_stop, stopped) = watch::channel(false);
    let state = api_state(mac.deps(), Duration::from_secs(15), stopped);
    let parts = |caller: Option<IrohCaller>| {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 7413));
        let mut request = Request::builder()
            .uri(PATH_AGENTS)
            .extension(ConnectInfo(address));
        if let Some(caller) = caller {
            request = request.extension(caller);
        }
        request.body(()).expect("request").into_parts().0
    };

    let refused = Identified::from_request_parts(&mut parts(None), &state).await;
    let Err(error) = refused else {
        panic!("an address identified a caller on the iroh server");
    };
    assert_eq!(error.0.code, ErrorCode::NotMember);

    let whois = whois_for(PeerKey(mac.endpoint.id()));
    let caller = Some(IrohCaller(whois.clone()));
    let identified = Identified::from_request_parts(&mut parts(caller), &state).await;
    assert_eq!(identified.ok().map(|identified| identified.0), Some(whois));
    mac.stop([]).await;
}
