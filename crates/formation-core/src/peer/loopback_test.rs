//! Two peer instances over loopback with a stub WhoIs (TASK-018 verification;
//! backend architecture, "Tests"): asks flow from A to B and back as events, the
//! outbox holds calls while B is down and delivers them once when it returns,
//! strangers get `403`, and a removed member is disconnected.
//!
//! Both instances run in this process on `127.0.0.1` with different ports. Every
//! connection comes from `127.0.0.1`, so each instance's stub WhoIs maps that
//! address to the other instance's member. The policy, roster, and ask and
//! request handlers are small doubles here; the real ones come with TASK-017,
//! TASK-019, and TASK-023.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::agent::{AgentState, Avatar, HarnessKind};
use formation_proto::ask::{Answer, AskTarget, Question, QuestionStatus};
use formation_proto::common::{
    AgentId, AnswerId, ApiError, ChainId, ErrorCode, MemberId, QuestionId, RequestId, Timestamp,
};
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerErrorBody, PeerEvent, PeerJoin, PeerJoinAccepted,
    PeerQuestionAction, PeerRequestCreate, SharedAgentView,
};
use formation_proto::policy::{Caller, Chain, PolicyVerdict, ToolCall, Visibility};
use formation_proto::request::{RequestStatus, WorkRequest};
use formation_proto::team::{Invitation, Member, MemberRole, WhoIs};

use super::api::{
    AskEndpoints, CallerIdentity, JoinEndpoint, PeerApi, PeerApiConfig, PeerApiDeps, PeerEventHub,
    RequestEndpoints, not_member,
};
use super::client::{PeerClient, PeerClientConfig, PeerDirectory};
use super::outbox::{MemoryOutboxStore, RetryPolicy, lock, now};
use crate::ports::{Peers, Policy, Result, TeamRepo, ToolContext};

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// WhoIs answers by caller IP; an unknown IP is not a tailnet user.
#[derive(Default)]
struct StubWhoIs {
    answers: Mutex<HashMap<IpAddr, WhoIs>>,
}

impl StubWhoIs {
    fn set(&self, ip: IpAddr, whois: Option<WhoIs>) {
        let mut answers = lock(&self.answers);
        match whois {
            Some(whois) => answers.insert(ip, whois),
            None => answers.remove(&ip),
        };
    }
}

#[async_trait]
impl CallerIdentity for StubWhoIs {
    async fn identify(&self, addr: SocketAddr) -> Result<WhoIs> {
        lock(&self.answers)
            .get(&addr.ip())
            .cloned()
            .ok_or_else(|| not_member(format!("no tailnet user at {addr}")))
    }
}

/// One Mac's roster and the teammates' addresses.
struct Roster {
    me: MemberId,
    members: Mutex<Vec<Member>>,
    addresses: Mutex<HashMap<MemberId, String>>,
}

impl TeamRepo for Roster {
    fn me(&self) -> Result<Option<MemberId>> {
        Ok(Some(self.me))
    }
    fn set_me(&self, _id: MemberId) -> Result<()> {
        Ok(())
    }
    fn team(&self) -> Result<Option<crate::ports::TeamRecord>> {
        Ok(None)
    }
    fn put_team(&self, _team: Option<&crate::ports::TeamRecord>) -> Result<()> {
        Ok(())
    }
    fn members(&self) -> Result<Vec<Member>> {
        Ok(lock(&self.members).clone())
    }
    fn member(&self, id: MemberId) -> Result<Option<Member>> {
        Ok(lock(&self.members).iter().find(|m| m.id == id).cloned())
    }
    fn member_by_tailscale(&self, login: &str) -> Result<Option<Member>> {
        Ok(lock(&self.members)
            .iter()
            .find(|m| m.tailscale_login == login)
            .cloned())
    }
    fn put_member(&self, member: &Member) -> Result<()> {
        let mut members = lock(&self.members);
        members.retain(|m| m.id != member.id);
        members.push(member.clone());
        Ok(())
    }
    fn remove_member(&self, id: MemberId, _at: Timestamp) -> Result<()> {
        lock(&self.members).retain(|m| m.id != id);
        Ok(())
    }
    fn put_invitation(&self, _invitation: &Invitation) -> Result<()> {
        Ok(())
    }
    fn invitation_by_code(&self, _code: &str) -> Result<Option<Invitation>> {
        Ok(None)
    }
}

impl PeerDirectory for Roster {
    fn me(&self) -> Option<MemberId> {
        Some(self.me)
    }
    fn members(&self) -> Vec<Member> {
        lock(&self.members).clone()
    }
    fn address(&self, member: MemberId) -> Option<String> {
        lock(&self.addresses).get(&member).cloned()
    }
}

/// Members by Tailscale login; one shared agent (accepts requests) and one
/// private agent.
struct TestPolicy {
    roster: Arc<Roster>,
    shared: SharedAgentView,
    private: AgentId,
}

#[async_trait]
impl Policy for TestPolicy {
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId> {
        self.roster
            .member_by_tailscale(&whois.login)?
            .map(|m| m.id)
            .ok_or_else(|| not_member(format!("{} is not a member", whois.login)))
    }
    async fn visible_agents(&self, _caller: &Caller) -> Result<Vec<SharedAgentView>> {
        Ok(vec![self.shared.clone()])
    }
    async fn check_ask(&self, _caller: &Caller, target: &AskTarget) -> Result<()> {
        match target {
            AskTarget::Agent { agent_id, .. } if *agent_id != self.shared.agent_id => Err(
                ApiError::new(ErrorCode::AgentNotShared, "this agent is not shared"),
            ),
            _ => Ok(()),
        }
    }
    async fn check_request(&self, _caller: &Caller, agent_id: AgentId) -> Result<()> {
        if agent_id == self.shared.agent_id {
            Ok(())
        } else {
            Err(ApiError::new(ErrorCode::AgentNotShared, "not shared"))
        }
    }
    async fn visibility(&self, _agent_id: AgentId) -> Result<Visibility> {
        Ok(Visibility::AnswerExcerpts)
    }
    async fn evaluate(&self, _context: &ToolContext, _call: &ToolCall) -> Result<PolicyVerdict> {
        Err(ApiError::new(ErrorCode::Internal, "not used here"))
    }
}

/// Answers every question at once and publishes it to the asker; replays return
/// the original.
struct TestAsk {
    hub: PeerEventHub,
    questions: Mutex<HashMap<QuestionId, Question>>,
    calls: Mutex<u32>,
}

impl TestAsk {
    fn unique(&self) -> usize {
        lock(&self.questions).len()
    }
}

#[async_trait]
impl AskEndpoints for TestAsk {
    async fn ask(&self, caller: Caller, ask: PeerAsk) -> Result<PeerAskAccepted> {
        *lock(&self.calls) += 1;
        let Caller::Member { member_id, .. } = caller else {
            panic!("peer callers are members");
        };
        let accepted = PeerAskAccepted {
            question_id: ask.question_id,
        };
        if lock(&self.questions).contains_key(&ask.question_id) {
            return Ok(accepted);
        }
        let at = now();
        let agent_id = match ask.target {
            AskTarget::Agent { agent_id, .. } => Some(agent_id),
            AskTarget::Person { .. } => None,
        };
        let question = Question {
            id: ask.question_id,
            thread_id: ask.thread_id.unwrap_or(ask.question_id),
            asker: member_id,
            target: ask.target,
            text: ask.text.clone(),
            status: QuestionStatus::Answered,
            candidates: Vec::new(),
            agent_id,
            answer: Some(Answer {
                id: AnswerId::generate(),
                agent_id,
                text: "Yes, the login API merged this morning.".into(),
                excerpts: Vec::new(),
                as_of: at,
                possibly_stale: false,
                answered_at: at,
            }),
            error: None,
            created_at: at,
            updated_at: at,
        };
        lock(&self.questions).insert(question.id, question.clone());
        self.hub.publish_to(
            member_id,
            PeerEvent::QuestionUpdate {
                question: Box::new(question),
            },
        );
        Ok(accepted)
    }

    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question> {
        let mut questions = lock(&self.questions);
        let question = questions
            .get_mut(&question_id)
            .filter(|q| q.asker == member)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such question"))?;
        if action == PeerQuestionAction::Cancel {
            question.status = QuestionStatus::Canceled;
        }
        Ok(question.clone())
    }

    async fn questions_to_resend(&self, member: MemberId) -> Result<Vec<Question>> {
        Ok(lock(&self.questions)
            .values()
            .filter(|q| q.asker == member)
            .cloned()
            .collect())
    }
}

/// Insert-or-return by (requester, idempotency key); publishes to the requester.
struct TestRequests {
    me: MemberId,
    hub: PeerEventHub,
    requests: Mutex<Vec<WorkRequest>>,
}

impl TestRequests {
    fn unique(&self) -> usize {
        lock(&self.requests).len()
    }

    fn update(
        &self,
        member: MemberId,
        id: RequestId,
        status: Option<RequestStatus>,
    ) -> Result<WorkRequest> {
        let mut requests = lock(&self.requests);
        let request = requests
            .iter_mut()
            .find(|r| r.id == id && r.requester == member)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "no such request"))?;
        if let Some(status) = status {
            request.status = status;
            request.updated_at = now();
        }
        Ok(request.clone())
    }
}

#[async_trait]
impl RequestEndpoints for TestRequests {
    async fn create(&self, caller: Caller, request: PeerRequestCreate) -> Result<WorkRequest> {
        let Caller::Member {
            member_id,
            via_agent,
            chain,
        } = caller
        else {
            panic!("peer callers are members");
        };
        let mut requests = lock(&self.requests);
        if let Some(existing) = requests
            .iter()
            .find(|r| r.requester == member_id && r.idempotency_key == request.idempotency_key)
        {
            return Ok(existing.clone());
        }
        let at = now();
        let created = WorkRequest {
            id: request.request_id,
            requester: member_id,
            requester_agent: via_agent,
            chain,
            owner: self.me,
            agent_id: request.agent_id,
            prompt: request.prompt,
            idempotency_key: request.idempotency_key,
            status: RequestStatus::Queued,
            result: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        requests.push(created.clone());
        self.hub.publish_to(
            member_id,
            PeerEvent::RequestUpdate {
                request: Box::new(created.clone()),
            },
        );
        Ok(created)
    }

    async fn get(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        self.update(member, id, None)
    }

    async fn cancel(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        self.update(member, id, Some(RequestStatus::Canceled))
    }
}

/// Accepts the code `fmn1-ok` and records the joiner from their WhoIs.
struct TestJoin {
    roster: Arc<Roster>,
}

#[async_trait]
impl JoinEndpoint for TestJoin {
    async fn join(&self, whois: WhoIs, join: PeerJoin) -> Result<PeerJoinAccepted> {
        if join.code != "fmn1-ok" {
            return Err(ApiError::new(ErrorCode::NotFound, "no such invitation"));
        }
        let mut member = member(&join.github_login, MemberRole::Member);
        member.tailscale_login = whois.login;
        member.tailscale_node = whois.node;
        self.roster.put_member(&member)?;
        Ok(PeerJoinAccepted {
            member_id: member.id,
            members: TeamRepo::members(self.roster.as_ref())?,
            team_id: None,
            version: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

fn member(login: &str, role: MemberRole) -> Member {
    Member {
        id: MemberId::generate(),
        github_login: login.into(),
        display_name: login.into(),
        tailscale_login: format!("{login}@github"),
        tailscale_node: format!("n{login}"),
        role,
        joined_at: now(),
        online: false,
        last_seen_at: None,
    }
}

fn whois_of(member: &Member) -> WhoIs {
    WhoIs {
        login: member.tailscale_login.clone(),
        node: member.tailscale_node.clone(),
    }
}

fn agent(owner: MemberId, slug: &str) -> SharedAgentView {
    SharedAgentView {
        owner,
        agent_id: AgentId::generate(),
        slug: slug.into(),
        name: slug.into(),
        role: "Backend developer".into(),
        harness: HarnessKind::ClaudeCode,
        avatar: Avatar {
            shape: "square".into(),
            color: "green".into(),
        },
        state: AgentState::Idle,
        last_activity_at: None,
        accepts_requests: true,
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind((LOOPBACK, 0))
        .and_then(|listener| listener.local_addr())
        .expect("free port")
        .port()
}

fn fast() -> RetryPolicy {
    RetryPolicy {
        initial: Duration::from_millis(50),
        max: Duration::from_millis(200),
        max_age: None,
    }
}

/// One Mac: its peer server (when running) and its client.
struct Instance {
    me: Member,
    port: u16,
    roster: Arc<Roster>,
    whois: Arc<StubWhoIs>,
    policy: Arc<TestPolicy>,
    ask: Arc<TestAsk>,
    requests: Arc<TestRequests>,
    hub: PeerEventHub,
    api: Option<PeerApi>,
    client: PeerClient,
}

impl Instance {
    fn new(me: &Member, mate: &Member, port: u16, mate_port: u16) -> Self {
        let roster = Arc::new(Roster {
            me: me.id,
            members: Mutex::new(vec![me.clone(), mate.clone()]),
            addresses: Mutex::new(HashMap::from([(mate.id, format!("127.0.0.1:{mate_port}"))])),
        });
        let whois = Arc::new(StubWhoIs::default());
        whois.set(LOOPBACK, Some(whois_of(mate)));
        let policy = Arc::new(TestPolicy {
            roster: roster.clone(),
            shared: agent(me.id, "api"),
            private: AgentId::generate(),
        });
        let hub = PeerEventHub::new();
        let ask = Arc::new(TestAsk {
            hub: hub.clone(),
            questions: Mutex::new(HashMap::new()),
            calls: Mutex::new(0),
        });
        let requests = Arc::new(TestRequests {
            me: me.id,
            hub: hub.clone(),
            requests: Mutex::new(Vec::new()),
        });
        let config = PeerClientConfig {
            connect_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(5),
            retry: fast(),
            reconnect: fast(),
            idle_timeout: Duration::from_secs(5),
        };
        let client = PeerClient::new(
            roster.clone(),
            Arc::new(MemoryOutboxStore::default()),
            config,
        )
        .expect("client");
        Self {
            me: me.clone(),
            port,
            roster,
            whois,
            policy,
            ask,
            requests,
            hub,
            api: None,
            client,
        }
    }

    fn id(&self) -> MemberId {
        self.me.id
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    async fn serve(&mut self) {
        let deps = PeerApiDeps {
            identity: self.whois.clone(),
            policy: self.policy.clone(),
            team: self.roster.clone(),
            join: Some(Arc::new(TestJoin {
                roster: self.roster.clone(),
            })),
            ask: Some(self.ask.clone()),
            requests: Some(self.requests.clone()),
            hub: self.hub.clone(),
        };
        let config = PeerApiConfig {
            bind: SocketAddr::new(LOOPBACK, self.port),
            heartbeat: Duration::from_millis(200),
        };
        self.api = Some(PeerApi::bind(config, deps).await.expect("bind"));
    }

    async fn stop(&mut self) {
        if let Some(api) = self.api.take() {
            api.shutdown().await.expect("shutdown");
        }
    }
}

/// A (creator) and B, both clients started; B's server only if `b_up`.
async fn pair(b_up: bool) -> (Instance, Instance) {
    let a_member = member("dohyeon", MemberRole::Creator);
    let b_member = member("minsu", MemberRole::Member);
    let (a_port, b_port) = (free_port(), free_port());
    let mut a = Instance::new(&a_member, &b_member, a_port, b_port);
    let mut b = Instance::new(&b_member, &a_member, b_port, a_port);
    a.serve().await;
    if b_up {
        b.serve().await;
    }
    a.client.start().expect("start a");
    b.client.start().expect("start b");
    (a, b)
}

fn peer_ask(owner: MemberId, agent_id: AgentId) -> PeerAsk {
    PeerAsk {
        question_id: QuestionId::generate(),
        thread_id: None,
        target: AskTarget::Agent {
            member_id: owner,
            agent_id,
        },
        text: "Is the login API done?".into(),
        via_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
    }
}

fn peer_request(agent_id: AgentId) -> PeerRequestCreate {
    PeerRequestCreate {
        request_id: RequestId::generate(),
        agent_id,
        prompt: "Add a health endpoint".into(),
        idempotency_key: format!("k-{}", RequestId::generate()),
        via_agent: Some("web".into()),
        chain: Chain {
            id: ChainId::generate(),
            depth: 1,
        },
    }
}

/// The next event matching `wanted`, skipping others.
async fn expect_event(
    events: &mut broadcast::Receiver<PeerEvent>,
    what: &str,
    wanted: impl Fn(&PeerEvent) -> bool,
) -> PeerEvent {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .expect("event");
        if wanted(&event) {
            return event;
        }
    }
}

fn is_presence(event: &PeerEvent, member: MemberId, online: bool) -> bool {
    matches!(event, PeerEvent::Presence { presence }
        if presence.member_id == member && presence.online == online)
}

fn is_question(event: &PeerEvent, id: QuestionId, status: QuestionStatus) -> bool {
    matches!(event, PeerEvent::QuestionUpdate { question }
        if question.id == id && question.status == status)
}

async fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn raw_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn error_of(response: reqwest::Response) -> (u16, ErrorCode) {
    let status = response.status().as_u16();
    let body: PeerErrorBody = response.json().await.expect("error body");
    (status, body.error.code)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ask_flows_from_a_to_b_and_the_answer_comes_back_as_an_event() {
    let (a, b) = pair(true).await;
    let mut events = a.client.subscribe();

    let hello = expect_event(&mut events, "B online", |e| is_presence(e, b.id(), true)).await;
    let PeerEvent::Presence { presence } = hello else {
        unreachable!()
    };
    assert_eq!(
        presence.agents,
        vec![b.policy.shared.clone()],
        "private agents never appear"
    );
    assert!(a.client.is_online(b.id()));

    let agents = a.client.agents(b.id()).await.unwrap();
    assert_eq!(agents, vec![b.policy.shared.clone()]);
    assert_eq!(a.client.roster(b.id()).await.unwrap().len(), 2);

    let ask = peer_ask(b.id(), b.policy.shared.agent_id);
    let accepted = a.client.ask(b.id(), ask.clone()).await.unwrap();
    assert_eq!(accepted.question_id, ask.question_id);
    let answered = expect_event(&mut events, "the answer", |e| {
        is_question(e, ask.question_id, QuestionStatus::Answered)
    })
    .await;
    let PeerEvent::QuestionUpdate { question } = answered else {
        unreachable!()
    };
    assert_eq!(question.asker, a.id());
    assert!(question.answer.is_some());

    // A retry with the same ID returns the same question; B asked once.
    let again = a.client.ask(b.id(), ask.clone()).await.unwrap();
    assert_eq!(again.question_id, ask.question_id);
    assert_eq!(b.ask.unique(), 1);
    assert_eq!(*lock(&b.ask.calls), 2);

    // A private agent is refused, and a refusal is final: nothing is queued.
    let private = peer_ask(b.id(), b.policy.private);
    let refused = a.client.ask(b.id(), private).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::AgentNotShared);
    // A question for another member's agent does not belong on B.
    let misrouted = a
        .client
        .ask(b.id(), peer_ask(a.id(), AgentId::generate()))
        .await;
    assert_eq!(misrouted.unwrap_err().code, ErrorCode::NotFound);
    assert!(a.client.outbox().pending(None).is_empty());

    let canceled = a
        .client
        .question_action(b.id(), ask.question_id, PeerQuestionAction::Cancel)
        .await
        .unwrap();
    assert_eq!(canceled.status, QuestionStatus::Canceled);

    // Work requests: create (a repeated key returns the original), read, cancel.
    let request = peer_request(b.policy.shared.agent_id);
    let created = a
        .client
        .create_request(b.id(), request.clone())
        .await
        .unwrap();
    assert_eq!(
        (created.id, created.requester),
        (request.request_id, a.id())
    );
    let repeat = PeerRequestCreate {
        request_id: RequestId::generate(),
        ..request.clone()
    };
    assert_eq!(
        a.client.create_request(b.id(), repeat).await.unwrap().id,
        request.request_id
    );
    assert_eq!(b.requests.unique(), 1);
    expect_event(
        &mut events,
        "the request update",
        |e| matches!(e, PeerEvent::RequestUpdate { request: r } if r.id == request.request_id),
    )
    .await;
    assert_eq!(
        a.client.request(b.id(), created.id).await.unwrap().status,
        RequestStatus::Queued
    );
    let canceled = a.client.cancel_request(b.id(), created.id).await.unwrap();
    assert_eq!(canceled.status, RequestStatus::Canceled);

    // The other direction works the same way.
    let agents = b.client.agents(a.id()).await.unwrap();
    assert_eq!(agents, vec![a.policy.shared.clone()]);

    // A member's malformed body is invalid_params, not a crash.
    let response = raw_client()
        .post(format!("http://{}/v1/ask", b.addr()))
        .header("content-type", "application/json")
        .body("{\"question_id\": 7}")
        .send()
        .await
        .unwrap();
    assert_eq!(error_of(response).await, (400, ErrorCode::InvalidParams));
}

#[tokio::test]
async fn the_outbox_holds_calls_while_b_is_down_and_delivers_them_once_when_b_returns() {
    let (a, mut b) = pair(false).await;
    let mut events = a.client.subscribe();
    expect_event(&mut events, "B offline", |e| is_presence(e, b.id(), false)).await;

    let ask = peer_ask(b.id(), b.policy.shared.agent_id);
    let waiting = a.client.ask(b.id(), ask.clone()).await.unwrap_err();
    assert_eq!(waiting.code, ErrorCode::PeerOffline);
    let request = peer_request(b.policy.shared.agent_id);
    let waiting = a
        .client
        .create_request(b.id(), request.clone())
        .await
        .unwrap_err();
    assert_eq!(waiting.code, ErrorCode::PeerOffline);
    // Asking again while it waits keeps one entry.
    assert!(a.client.ask(b.id(), ask.clone()).await.is_err());
    assert_eq!(a.client.outbox().pending(Some(b.id())).len(), 2);
    assert_eq!(
        a.client
            .request(b.id(), request.request_id)
            .await
            .unwrap_err()
            .code,
        ErrorCode::PeerOffline
    );

    // Retries keep failing while B is down.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.client.outbox().pending(Some(b.id()))[0].attempts >= 1);
    assert_eq!(*lock(&b.ask.calls), 0);

    b.serve().await;
    expect_event(&mut events, "B back online", |e| {
        is_presence(e, b.id(), true)
    })
    .await;
    expect_event(&mut events, "the answer to the waiting question", |e| {
        is_question(e, ask.question_id, QuestionStatus::Answered)
    })
    .await;
    until("the outbox to empty", || {
        a.client.outbox().pending(None).is_empty()
    })
    .await;
    assert_eq!(b.ask.unique(), 1, "delivered once");
    assert_eq!(b.requests.unique(), 1, "delivered once");
    let delivered = lock(&b.requests.requests)[0].clone();
    assert_eq!(delivered.id, request.request_id);
    assert_eq!(delivered.chain, request.chain);
}

#[tokio::test]
async fn calls_made_while_b_restarts_wait_and_arrive_after() {
    let (a, mut b) = pair(true).await;
    let mut events = a.client.subscribe();
    expect_event(&mut events, "B online", |e| is_presence(e, b.id(), true)).await;

    b.stop().await;
    let gone = expect_event(&mut events, "B offline", |e| is_presence(e, b.id(), false)).await;
    let PeerEvent::Presence { presence } = gone else {
        unreachable!()
    };
    assert!(presence.last_seen_at.is_some());
    assert_eq!(presence.agents.len(), 1, "last seen agents stay");

    let ask = peer_ask(b.id(), b.policy.shared.agent_id);
    assert_eq!(
        a.client.ask(b.id(), ask.clone()).await.unwrap_err().code,
        ErrorCode::PeerOffline
    );
    // A waiting call can be canceled locally; B never sees it.
    let dropped = peer_ask(b.id(), b.policy.shared.agent_id);
    assert!(a.client.ask(b.id(), dropped.clone()).await.is_err());
    let canceled = a
        .client
        .question_action(b.id(), dropped.question_id, PeerQuestionAction::Cancel)
        .await
        .unwrap();
    assert_eq!(canceled.status, QuestionStatus::Canceled);

    b.serve().await;
    expect_event(&mut events, "B online again", |e| {
        is_presence(e, b.id(), true)
    })
    .await;
    expect_event(&mut events, "the answer", |e| {
        is_question(e, ask.question_id, QuestionStatus::Answered)
    })
    .await;
    until("the outbox to empty", || {
        a.client.outbox().pending(None).is_empty()
    })
    .await;
    assert_eq!(b.ask.unique(), 1);
    assert!(!lock(&b.ask.questions).contains_key(&dropped.question_id));
}

#[tokio::test]
async fn strangers_get_403_on_every_endpoint_except_join() {
    let (a, b) = pair(true).await;
    // B no longer recognizes the caller at 127.0.0.1 as a member.
    let stranger = WhoIs {
        login: "eve@github".into(),
        node: "neve".into(),
    };
    b.whois.set(LOOPBACK, Some(stranger));

    let http = raw_client();
    let base = format!("http://{}", b.addr());
    let ask = serde_json::to_value(peer_ask(b.id(), b.policy.shared.agent_id)).unwrap();
    let request = serde_json::to_value(peer_request(b.policy.shared.agent_id)).unwrap();
    let id = RequestId::generate();
    let calls = [
        http.get(format!("{base}/v1/agents")),
        http.post(format!("{base}/v1/ask")).json(&ask),
        http.post(format!("{base}/v1/ask")).body("not json"),
        http.post(format!(
            "{base}/v1/questions/{}/action",
            QuestionId::generate()
        ))
        .json(&serde_json::json!({"action": "cancel"})),
        http.post(format!("{base}/v1/requests")).json(&request),
        http.get(format!("{base}/v1/requests/{id}")),
        http.post(format!("{base}/v1/requests/{id}/cancel")),
        http.get(format!("{base}/v1/team")),
        http.get(format!("{base}/v1/events")),
        http.get(format!("{base}/v1/no-such-endpoint")),
        http.delete(format!("{base}/v1/agents")),
    ];
    for call in calls {
        let response = call.send().await.unwrap();
        let url = response.url().to_string();
        assert_eq!(
            error_of(response).await,
            (403, ErrorCode::NotMember),
            "{url}"
        );
    }
    let refused = tokio_tungstenite::connect_async(format!("ws://{}/v1/events", b.addr())).await;
    match refused {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status().as_u16(), 403)
        }
        other => panic!("expected 403, got {other:?}"),
    }

    // Through the client: refused at once, and not queued.
    let ask = peer_ask(b.id(), b.policy.shared.agent_id);
    assert_eq!(
        a.client.ask(b.id(), ask).await.unwrap_err().code,
        ErrorCode::NotMember
    );
    assert!(a.client.outbox().pending(None).is_empty());

    // A caller Tailscale cannot identify, and this Mac's own member, are refused too.
    b.whois.set(LOOPBACK, None);
    let response = http.get(format!("{base}/v1/agents")).send().await.unwrap();
    assert_eq!(error_of(response).await, (403, ErrorCode::NotMember));
    b.whois.set(LOOPBACK, Some(whois_of(&b.me)));
    let response = http.get(format!("{base}/v1/agents")).send().await.unwrap();
    assert_eq!(error_of(response).await, (403, ErrorCode::NotMember));

    // Join is the one call a non-member may make; the code is the check. B is not
    // the creator here, but the double serves join on every Mac.
    b.whois.set(
        LOOPBACK,
        Some(WhoIs {
            login: "jisoo@github".into(),
            node: "njisoo".into(),
        }),
    );
    let join = PeerJoin {
        code: "fmn1-ok".into(),
        github_login: "jisoo".into(),
        display_name: "Jisoo".into(),
    };
    let accepted = a.client.join(&b.addr(), join.clone()).await.unwrap();
    assert_eq!(accepted.members.len(), 3);
    let joined = b.roster.member(accepted.member_id).unwrap().unwrap();
    assert_eq!(
        joined.tailscale_login, "jisoo@github",
        "recorded from WhoIs"
    );
    let wrong = PeerJoin {
        code: "fmn1-nope".into(),
        ..join
    };
    assert_eq!(
        a.client.join(&b.addr(), wrong).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    // Now a member, the joiner reaches the other endpoints.
    let response = http.get(format!("{base}/v1/team")).send().await.unwrap();
    assert_eq!(response.status().as_u16(), 200);
}

#[tokio::test]
async fn a_removed_member_is_disconnected_and_refused() {
    let (a, b) = pair(true).await;
    let mut events = a.client.subscribe();
    expect_event(&mut events, "B online", |e| is_presence(e, b.id(), true)).await;
    until("A's events connection on B", || b.hub.connections() >= 1).await;

    b.roster.remove_member(a.id(), now()).unwrap();
    b.hub.publish_all(PeerEvent::Roster {
        members: TeamRepo::members(b.roster.as_ref()).unwrap(),
        version: 0,
    });

    expect_event(&mut events, "B closing A's connection", |e| {
        is_presence(e, b.id(), false)
    })
    .await;
    assert_eq!(
        a.client.agents(b.id()).await.unwrap_err().code,
        ErrorCode::NotMember
    );
    // Reconnects are refused too, so A stays offline.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!a.client.is_online(b.id()));
}

#[tokio::test]
async fn roster_events_count_only_from_the_creator() {
    let (a, b) = pair(true).await;
    let mut a_events = a.client.subscribe();
    let mut b_events = b.client.subscribe();
    expect_event(&mut a_events, "B online", |e| is_presence(e, b.id(), true)).await;
    expect_event(&mut b_events, "A online", |e| is_presence(e, a.id(), true)).await;

    // B is not the creator: A drops B's roster event.
    b.hub.publish_all(PeerEvent::Roster {
        members: Vec::new(),
        version: 0,
    });
    // A is: B takes A's.
    a.hub.publish_all(PeerEvent::Roster {
        members: TeamRepo::members(a.roster.as_ref()).unwrap(),
        version: 0,
    });
    let roster = expect_event(&mut b_events, "A's roster", |e| {
        matches!(e, PeerEvent::Roster { .. })
    })
    .await;
    assert!(matches!(roster, PeerEvent::Roster { members, .. } if members.len() == 2));

    // B's connection to A carries events in order: once an answer B published
    // after its roster event reaches A, the roster event would have come first.
    let ask = peer_ask(b.id(), b.policy.shared.agent_id);
    a.client.ask(b.id(), ask.clone()).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let event = tokio::time::timeout_at(deadline, a_events.recv())
            .await
            .expect("the answer in time")
            .expect("event");
        assert!(
            !matches!(event, PeerEvent::Roster { .. }),
            "B's roster must be dropped"
        );
        if is_question(&event, ask.question_id, QuestionStatus::Answered) {
            break;
        }
    }
}
