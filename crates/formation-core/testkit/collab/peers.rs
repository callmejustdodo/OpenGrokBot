//! Owner: B. `FakePeers`: teammates' Macs simulated in memory, with presence,
//! an outbox for offline teammates, and scripted answers.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::ask::{
    Answer, AskTarget, Question, QuestionStatus, RouteChoice, RoutingCandidate,
};
use formation_proto::common::{
    AgentId, AnswerId, ApiError, ErrorCode, MemberId, QuestionId, RequestId, Timestamp,
};
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerEvent, PeerJoin, PeerJoinAccepted, PeerQuestionAction,
    PeerRequestCreate, PresenceEvent, SharedAgentView,
};
use formation_proto::request::{RequestStatus, WorkRequest};
use formation_proto::team::{Member, MemberRole, WhoIs};

use crate::ports::{Peers, Result};

use super::{error, lock, now};

/// How a teammate's Mac answers the next question.
#[derive(Debug, Clone, PartialEq)]
pub enum AskReply {
    /// `received`: accepted, no answer yet (the default).
    Received,
    /// `answering` with the agent that answers.
    Answering { agent_id: AgentId },
    /// `unsure`: routing was not confident; the asker picks (D-027).
    Unsure(Vec<RoutingCandidate>),
    /// `answered` at once.
    Answered { agent_id: AgentId, text: String },
}

/// What waits in the outbox while a teammate is offline (FR-016).
#[derive(Debug, Clone, PartialEq)]
pub enum OutboxItem {
    Ask {
        member: MemberId,
        ask: PeerAsk,
    },
    Request {
        member: MemberId,
        request: PeerRequestCreate,
    },
}

impl OutboxItem {
    fn member(&self) -> MemberId {
        match self {
            Self::Ask { member, .. } | Self::Request { member, .. } => *member,
        }
    }
}

/// One call to the fake, with its arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum PeersCall {
    Join {
        creator_address: String,
        join: PeerJoin,
    },
    Roster(MemberId),
    Agents(MemberId),
    Ask {
        member: MemberId,
        ask: PeerAsk,
    },
    QuestionAction {
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    },
    CreateRequest {
        member: MemberId,
        request: PeerRequestCreate,
    },
    Request {
        member: MemberId,
        id: RequestId,
    },
    CancelRequest {
        member: MemberId,
        id: RequestId,
    },
}

/// A [`Peers`] client whose teammates live in memory.
///
/// Calls to a member not added with [`FakePeers::add_teammate`] get `not_found`.
/// An offline teammate gives `peer_offline`; for `ask` and `create_request` the
/// call also waits in the outbox (once per question ID or idempotency key) and is
/// delivered by [`FakePeers::set_online`]. On delivery the teammate's Mac checks
/// what the real owner's Mac checks: the target belongs to that member
/// (`invalid_params`), the agent is shared (`agent_not_shared`) and, for requests,
/// accepts them (`requests_not_accepted`). Retries are idempotent: the same
/// question ID or idempotency key returns the original.
///
/// Questions arrive as [`AskReply::Received`] unless scripted; requests arrive
/// `queued` unless set with [`FakePeers::set_arrival_status`]. Tests move them on
/// with [`FakePeers::answer`], [`FakePeers::set_question_status`], and
/// [`FakePeers::set_request_status`]; every change is published on
/// [`Peers::subscribe`], as the teammate's `WS /v1/events` would, in the order it
/// happened, also across threads.
pub struct FakePeers {
    me: MemberId,
    state: Mutex<PeersState>,
    events: broadcast::Sender<PeerEvent>,
}

#[derive(Default)]
struct PeersState {
    teammates: HashMap<MemberId, Teammate>,
    roster: Vec<Member>,
    invitations: Vec<Invite>,
    outbox: Vec<OutboxItem>,
    calls: Vec<PeersCall>,
}

struct Teammate {
    online: bool,
    last_seen_at: Option<Timestamp>,
    agents: Vec<SharedAgentView>,
    questions: Vec<Question>,
    requests: Vec<WorkRequest>,
    replies: VecDeque<AskReply>,
    arrival_status: RequestStatus,
    failures: VecDeque<ApiError>,
}

struct Invite {
    address: String,
    code: String,
    joiner: WhoIs,
    used: bool,
}

impl std::fmt::Debug for FakePeers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = lock(&self.state);
        f.debug_struct("FakePeers")
            .field("me", &self.me)
            .field("teammates", &state.teammates.len())
            .field("outbox", &state.outbox)
            .finish_non_exhaustive()
    }
}

impl FakePeers {
    /// A client on the Mac of member `me`, who appears as the asker and requester.
    pub fn new(me: MemberId) -> Self {
        Self {
            me,
            state: Mutex::default(),
            events: broadcast::channel(256).0,
        }
    }

    pub fn me(&self) -> MemberId {
        self.me
    }

    /// Adds an online teammate with the agents they share with me.
    pub fn add_teammate(&self, member_id: MemberId, agents: Vec<SharedAgentView>) {
        lock(&self.state).teammates.insert(
            member_id,
            Teammate {
                online: true,
                last_seen_at: Some(now()),
                agents,
                questions: vec![],
                requests: vec![],
                replies: VecDeque::new(),
                arrival_status: RequestStatus::Queued,
                failures: VecDeque::new(),
            },
        );
    }

    /// Replaces the teammate's shared agents and publishes their presence.
    pub fn set_agents(&self, member_id: MemberId, agents: Vec<SharedAgentView>) -> Result<()> {
        let mut state = lock(&self.state);
        let teammate = state.teammate(member_id)?;
        teammate.agents = agents;
        let event = teammate.presence(member_id);
        self.publish(&state, event);
        Ok(())
    }

    /// Replaces the roster every Mac serves and publishes it.
    pub fn set_roster(&self, members: Vec<Member>) {
        let mut state = lock(&self.state);
        state.roster = members.clone();
        self.publish(&state, PeerEvent::Roster { members });
    }

    /// Takes the teammate's Mac offline or back online and publishes their
    /// presence. Coming back delivers their outbox items in order and returns each
    /// with its delivery result; delivered or rejected, they leave the outbox.
    pub fn set_online(
        &self,
        member_id: MemberId,
        online: bool,
    ) -> Result<Vec<(OutboxItem, Result<()>)>> {
        let mut state = lock(&self.state);
        let teammate = state.teammate(member_id)?;
        teammate.online = online;
        teammate.last_seen_at = Some(now());
        let mut events = vec![teammate.presence(member_id)];
        let mut delivered = vec![];
        if online {
            let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut state.outbox)
                .into_iter()
                .partition(|item| item.member() == member_id);
            state.outbox = rest;
            let teammate = state.teammate(member_id)?;
            for item in mine {
                let outcome = match &item {
                    OutboxItem::Ask { ask, .. } => teammate
                        .receive_ask(self.me, member_id, ask)
                        .map(|question| events.extend(question.map(question_event))),
                    OutboxItem::Request { request, .. } => teammate
                        .receive_request(self.me, member_id, request)
                        .map(|(request, new)| {
                            if new {
                                events.push(request_event(request));
                            }
                        }),
                };
                delivered.push((item, outcome));
            }
        }
        events
            .into_iter()
            .for_each(|event| self.publish(&state, event));
        Ok(delivered)
    }

    /// Answers the teammate's next question with `reply`. Replies queue up.
    pub fn script_ask(&self, member_id: MemberId, reply: AskReply) -> Result<()> {
        lock(&self.state)
            .teammate(member_id)?
            .replies
            .push_back(reply);
        Ok(())
    }

    /// The status the teammate's new requests start in (`queued` by default).
    pub fn set_arrival_status(&self, member_id: MemberId, status: RequestStatus) -> Result<()> {
        lock(&self.state).teammate(member_id)?.arrival_status = status;
        Ok(())
    }

    /// The next call to the teammate fails with `error`. Failures queue up.
    pub fn fail_next(&self, member_id: MemberId, error: ApiError) -> Result<()> {
        lock(&self.state)
            .teammate(member_id)?
            .failures
            .push_back(error);
        Ok(())
    }

    /// Lets [`Peers::join`] at `creator_address` succeed once with `code`; the new
    /// member gets the Tailscale identity in `joiner`, as the creator's Mac records
    /// it from WhoIs, and my ID ([`FakePeers::me`]), so the questions and requests
    /// this client sends afterwards come from the member the join returned.
    pub fn expect_join(&self, creator_address: &str, code: &str, joiner: WhoIs) {
        lock(&self.state).invitations.push(Invite {
            address: creator_address.into(),
            code: code.into(),
            joiner,
            used: false,
        });
    }

    /// The owner's agent answers the question.
    pub fn answer(
        &self,
        question_id: QuestionId,
        agent_id: AgentId,
        text: &str,
    ) -> Result<Question> {
        self.update_question(question_id, |question| {
            let at = now();
            question.status = QuestionStatus::Answered;
            question.agent_id = Some(agent_id);
            question.candidates.clear();
            question.answer = Some(Answer {
                id: AnswerId::generate(),
                agent_id: Some(agent_id),
                text: text.into(),
                excerpts: vec![],
                as_of: at,
                possibly_stale: false,
                answered_at: at,
            });
        })
    }

    pub fn set_question_status(
        &self,
        question_id: QuestionId,
        status: QuestionStatus,
    ) -> Result<Question> {
        self.update_question(question_id, |question| question.status = status)
    }

    /// Moves a request on as the owner's Mac would. `conflict` once it is final.
    pub fn set_request_status(
        &self,
        request_id: RequestId,
        status: RequestStatus,
        result: Option<String>,
    ) -> Result<WorkRequest> {
        let mut state = lock(&self.state);
        let request = state
            .teammates
            .values_mut()
            .flat_map(|teammate| teammate.requests.iter_mut())
            .find(|request| request.id == request_id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no request {request_id}")))?;
        if request.status.is_final() {
            return Err(error(
                ErrorCode::Conflict,
                format!("request {request_id} is already {:?}", request.status),
            ));
        }
        request.status = status;
        if result.is_some() {
            request.result = result;
        }
        request.updated_at = now();
        let request = request.clone();
        self.publish(&state, request_event(request.clone()));
        Ok(request)
    }

    /// The questions on the teammate's Mac.
    pub fn questions(&self, member_id: MemberId) -> Vec<Question> {
        lock(&self.state)
            .teammates
            .get(&member_id)
            .map(|teammate| teammate.questions.clone())
            .unwrap_or_default()
    }

    /// The requests on the teammate's Mac.
    pub fn requests(&self, member_id: MemberId) -> Vec<WorkRequest> {
        lock(&self.state)
            .teammates
            .get(&member_id)
            .map(|teammate| teammate.requests.clone())
            .unwrap_or_default()
    }

    pub fn outbox(&self) -> Vec<OutboxItem> {
        lock(&self.state).outbox.clone()
    }

    pub fn calls(&self) -> Vec<PeersCall> {
        lock(&self.state).calls.clone()
    }

    fn update_question(
        &self,
        question_id: QuestionId,
        change: impl FnOnce(&mut Question),
    ) -> Result<Question> {
        let mut state = lock(&self.state);
        let question = state
            .teammates
            .values_mut()
            .flat_map(|teammate| teammate.questions.iter_mut())
            .find(|question| question.id == question_id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no question {question_id}")))?;
        change(question);
        question.updated_at = now();
        let question = question.clone();
        self.publish(&state, question_event(question.clone()));
        Ok(question)
    }

    /// Sends `event` while the caller still holds the state lock (`_state`), so
    /// changes made on different threads reach subscribers in the order they
    /// happened. `broadcast::Sender::send` never takes our lock.
    fn publish(&self, _state: &PeersState, event: PeerEvent) {
        // No subscriber is fine; events are not kept.
        let _ = self.events.send(event);
    }
}

fn question_event(question: Question) -> PeerEvent {
    PeerEvent::QuestionUpdate {
        question: Box::new(question),
    }
}

fn request_event(request: WorkRequest) -> PeerEvent {
    PeerEvent::RequestUpdate {
        request: Box::new(request),
    }
}

impl PeersState {
    fn teammate(&mut self, member_id: MemberId) -> Result<&mut Teammate> {
        let teammate = self
            .teammates
            .get_mut(&member_id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no teammate {member_id}")))?;
        Ok(teammate)
    }

    /// The teammate for a port call, after any scripted failure.
    fn call(&mut self, member_id: MemberId) -> Result<&mut Teammate> {
        let teammate = self.teammate(member_id)?;
        if let Some(error) = teammate.failures.pop_front() {
            return Err(error);
        }
        Ok(teammate)
    }

    /// As [`PeersState::call`], and `peer_offline` when the teammate is offline.
    fn reachable(&mut self, member_id: MemberId) -> Result<&mut Teammate> {
        let teammate = self.call(member_id)?;
        if !teammate.online {
            return Err(offline(member_id));
        }
        Ok(teammate)
    }

    fn queue(&mut self, item: OutboxItem) {
        let duplicate = self.outbox.iter().any(|queued| match (queued, &item) {
            (OutboxItem::Ask { ask: a, .. }, OutboxItem::Ask { ask: b, .. }) => {
                a.question_id == b.question_id
            }
            (
                OutboxItem::Request {
                    member: m,
                    request: a,
                },
                OutboxItem::Request {
                    member: n,
                    request: b,
                },
            ) => m == n && a.idempotency_key == b.idempotency_key,
            _ => false,
        });
        if !duplicate {
            self.outbox.push(item);
        }
    }
}

fn offline(member_id: MemberId) -> ApiError {
    error(
        ErrorCode::PeerOffline,
        format!("{member_id}'s Mac is offline"),
    )
}

fn shared(agents: &[SharedAgentView], agent_id: AgentId) -> Result<&SharedAgentView> {
    agents
        .iter()
        .find(|view| view.agent_id == agent_id)
        .ok_or_else(|| {
            error(
                ErrorCode::AgentNotShared,
                format!("agent {agent_id} is not shared"),
            )
        })
}

impl Teammate {
    fn presence(&self, member_id: MemberId) -> PeerEvent {
        PeerEvent::Presence {
            presence: PresenceEvent {
                member_id,
                online: self.online,
                last_seen_at: self.last_seen_at,
                agents: self.agents.clone(),
            },
        }
    }

    /// The new question, or `None` for a retry of one already received.
    fn receive_ask(
        &mut self,
        me: MemberId,
        member_id: MemberId,
        ask: &PeerAsk,
    ) -> Result<Option<Question>> {
        if ask.target.owner() != member_id {
            return Err(error(
                ErrorCode::InvalidParams,
                format!(
                    "the question is for {}, not {member_id}",
                    ask.target.owner()
                ),
            ));
        }
        if self.questions.iter().any(|q| q.id == ask.question_id) {
            return Ok(None);
        }
        let target_agent = match ask.target {
            AskTarget::Agent { agent_id, .. } => Some(shared(&self.agents, agent_id)?.agent_id),
            AskTarget::Person { .. } => None,
        };
        let at = now();
        let mut question = Question {
            id: ask.question_id,
            thread_id: ask.thread_id.unwrap_or(ask.question_id),
            asker: me,
            target: ask.target,
            text: ask.text.clone(),
            status: QuestionStatus::Received,
            candidates: vec![],
            agent_id: target_agent,
            answer: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        match self.replies.pop_front().unwrap_or(AskReply::Received) {
            AskReply::Received => {}
            AskReply::Answering { agent_id } => {
                question.status = QuestionStatus::Answering;
                question.agent_id = Some(agent_id);
            }
            AskReply::Unsure(candidates) => {
                question.status = QuestionStatus::Unsure;
                question.candidates = candidates;
            }
            AskReply::Answered { agent_id, text } => {
                question.status = QuestionStatus::Answered;
                question.agent_id = Some(agent_id);
                question.answer = Some(Answer {
                    id: AnswerId::generate(),
                    agent_id: Some(agent_id),
                    text,
                    excerpts: vec![],
                    as_of: at,
                    possibly_stale: false,
                    answered_at: at,
                });
            }
        }
        self.questions.push(question.clone());
        Ok(Some(question))
    }

    /// The request and whether it is new; a repeated idempotency key returns the original.
    fn receive_request(
        &mut self,
        me: MemberId,
        member_id: MemberId,
        create: &PeerRequestCreate,
    ) -> Result<(WorkRequest, bool)> {
        if let Some(existing) = self
            .requests
            .iter()
            .find(|r| r.idempotency_key == create.idempotency_key)
        {
            return Ok((existing.clone(), false));
        }
        if !shared(&self.agents, create.agent_id)?.accepts_requests {
            return Err(error(
                ErrorCode::RequestsNotAccepted,
                format!("agent {} does not accept requests", create.agent_id),
            ));
        }
        let at = now();
        let request = WorkRequest {
            id: create.request_id,
            requester: me,
            requester_agent: create.via_agent.clone(),
            chain: create.chain,
            owner: member_id,
            agent_id: create.agent_id,
            prompt: create.prompt.clone(),
            idempotency_key: create.idempotency_key.clone(),
            status: self.arrival_status,
            result: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        self.requests.push(request.clone());
        Ok((request, true))
    }
}

#[async_trait]
impl Peers for FakePeers {
    /// `peer_offline` when nothing was expected at the address, `not_found` for an
    /// unknown code, `conflict` for a used code, or when I or the GitHub login are
    /// already in the team.
    async fn join(&self, creator_address: &str, join: PeerJoin) -> Result<PeerJoinAccepted> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::Join {
            creator_address: creator_address.into(),
            join: join.clone(),
        });
        if !state
            .invitations
            .iter()
            .any(|i| i.address == creator_address)
        {
            return Err(error(
                ErrorCode::PeerOffline,
                format!("no formationd answers at {creator_address}"),
            ));
        }
        if state
            .roster
            .iter()
            .any(|m| m.id == self.me || m.github_login == join.github_login)
        {
            return Err(error(
                ErrorCode::Conflict,
                format!("{} is already in the team", join.github_login),
            ));
        }
        let invite = state
            .invitations
            .iter_mut()
            .find(|i| i.address == creator_address && i.code == join.code)
            .ok_or_else(|| error(ErrorCode::NotFound, "no such invitation"))?;
        if invite.used {
            return Err(error(
                ErrorCode::Conflict,
                "the invitation was already used",
            ));
        }
        invite.used = true;
        let at = now();
        let member = Member {
            id: self.me,
            github_login: join.github_login,
            display_name: join.display_name,
            tailscale_login: invite.joiner.login.clone(),
            tailscale_node: invite.joiner.node.clone(),
            role: MemberRole::Member,
            joined_at: at,
            online: true,
            last_seen_at: Some(at),
        };
        state.roster.push(member.clone());
        let members = state.roster.clone();
        self.publish(
            &state,
            PeerEvent::Roster {
                members: members.clone(),
            },
        );
        Ok(PeerJoinAccepted {
            member_id: member.id,
            members,
        })
    }

    async fn roster(&self, member: MemberId) -> Result<Vec<Member>> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::Roster(member));
        state.reachable(member)?;
        Ok(state.roster.clone())
    }

    async fn agents(&self, member: MemberId) -> Result<Vec<SharedAgentView>> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::Agents(member));
        Ok(state.reachable(member)?.agents.clone())
    }

    async fn ask(&self, member: MemberId, ask: PeerAsk) -> Result<PeerAskAccepted> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::Ask {
            member,
            ask: ask.clone(),
        });
        let teammate = state.call(member)?;
        if !teammate.online {
            state.queue(OutboxItem::Ask { member, ask });
            return Err(offline(member));
        }
        let question = teammate.receive_ask(self.me, member, &ask)?;
        if let Some(question) = question {
            self.publish(&state, question_event(question));
        }
        Ok(PeerAskAccepted {
            question_id: ask.question_id,
        })
    }

    /// `conflict` when a canceled question gets another action, a question that
    /// is already answered, canceled, or failed is canceled, or a question that is
    /// not answered or failed is refreshed.
    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::QuestionAction {
            member,
            question_id,
            action,
        });
        let Teammate {
            questions, agents, ..
        } = state.reachable(member)?;
        let question = questions
            .iter_mut()
            .find(|q| q.id == question_id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no question {question_id}")))?;
        let conflict = || {
            error(
                ErrorCode::Conflict,
                format!("question {question_id} is {:?}", question.status),
            )
        };
        let status = match action {
            _ if question.status == QuestionStatus::Canceled => return Err(conflict()),
            PeerQuestionAction::Cancel => match question.status {
                QuestionStatus::Answered | QuestionStatus::Failed => return Err(conflict()),
                _ => QuestionStatus::Canceled,
            },
            PeerQuestionAction::Refresh => match question.status {
                QuestionStatus::Answered | QuestionStatus::Failed => QuestionStatus::Answering,
                _ => return Err(conflict()),
            },
            PeerQuestionAction::Route {
                choice: RouteChoice::Agent { agent_id },
            } => {
                shared(agents, agent_id)?;
                question.agent_id = Some(agent_id);
                question.candidates.clear();
                QuestionStatus::Answering
            }
            PeerQuestionAction::Route {
                choice: RouteChoice::Owner,
            } => QuestionStatus::SentToOwner,
        };
        question.status = status;
        question.updated_at = now();
        let question = question.clone();
        self.publish(&state, question_event(question.clone()));
        Ok(question)
    }

    async fn create_request(
        &self,
        member: MemberId,
        request: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::CreateRequest {
            member,
            request: request.clone(),
        });
        let teammate = state.call(member)?;
        if !teammate.online {
            state.queue(OutboxItem::Request { member, request });
            return Err(offline(member));
        }
        let (request, new) = teammate.receive_request(self.me, member, &request)?;
        if new {
            self.publish(&state, request_event(request.clone()));
        }
        Ok(request)
    }

    async fn request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::Request { member, id });
        state
            .reachable(member)?
            .requests
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no request {id}")))
    }

    /// Cancels a request that is `received` or `queued`; `conflict` otherwise.
    async fn cancel_request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        let mut state = lock(&self.state);
        state.calls.push(PeersCall::CancelRequest { member, id });
        let request = state
            .reachable(member)?
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| error(ErrorCode::NotFound, format!("no request {id}")))?;
        if !matches!(
            request.status,
            RequestStatus::Received | RequestStatus::Queued
        ) {
            return Err(error(
                ErrorCode::Conflict,
                format!(
                    "request {id} is {:?} and can no longer be canceled",
                    request.status
                ),
            ));
        }
        request.status = RequestStatus::Canceled;
        request.updated_at = now();
        let request = request.clone();
        self.publish(&state, request_event(request.clone()));
        Ok(request)
    }

    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use formation_proto::common::ChainId;
    use formation_proto::policy::Chain;

    use super::super::block_on;
    use super::super::samples::{member, shared_agent, whois_of};
    use super::*;

    struct World {
        peers: Arc<FakePeers>,
        dohyeon: MemberId,
        api: SharedAgentView,
        web: SharedAgentView,
    }

    /// Minsu's Mac, with Dohyeon sharing `api` (accepts requests) and `web` (does not).
    fn world() -> World {
        let peers = Arc::new(FakePeers::new(MemberId::generate()));
        let dohyeon = MemberId::generate();
        let api = shared_agent(dohyeon, "api", true);
        let web = shared_agent(dohyeon, "web", false);
        peers.add_teammate(dohyeon, vec![api.clone(), web.clone()]);
        World {
            peers,
            dohyeon,
            api,
            web,
        }
    }

    fn chain() -> Chain {
        Chain {
            id: ChainId::generate(),
            depth: 0,
        }
    }

    fn ask(target: AskTarget) -> PeerAsk {
        PeerAsk {
            question_id: QuestionId::generate(),
            thread_id: None,
            target,
            text: "How far is the login work?".into(),
            via_agent: None,
            chain: chain(),
        }
    }

    fn create(agent_id: AgentId, key: &str) -> PeerRequestCreate {
        PeerRequestCreate {
            request_id: RequestId::generate(),
            agent_id,
            prompt: "Add token expiry tests".into(),
            idempotency_key: key.into(),
            via_agent: None,
            chain: chain(),
        }
    }

    #[test]
    fn questions_arrive_once_and_follow_scripted_replies() {
        let World {
            peers,
            dohyeon,
            api,
            ..
        } = world();
        let port: Arc<dyn Peers> = peers.clone();
        let mut events = port.subscribe();
        block_on(async {
            assert_eq!(port.agents(dohyeon).await.unwrap().len(), 2);

            let first = ask(AskTarget::Person { member_id: dohyeon });
            let accepted = port.ask(dohyeon, first.clone()).await.unwrap();
            assert_eq!(accepted.question_id, first.question_id);
            port.ask(dohyeon, first.clone()).await.unwrap();
            assert_eq!(peers.questions(dohyeon).len(), 1);
            let PeerEvent::QuestionUpdate { question } = events.recv().await.unwrap() else {
                panic!("expected a question update");
            };
            assert_eq!(question.status, QuestionStatus::Received);
            assert_eq!(question.asker, peers.me());
            assert_eq!(question.thread_id, first.question_id);

            peers
                .script_ask(
                    dohyeon,
                    AskReply::Answered {
                        agent_id: api.agent_id,
                        text: "Merged this morning.".into(),
                    },
                )
                .unwrap();
            let target = AskTarget::Agent {
                member_id: dohyeon,
                agent_id: api.agent_id,
            };
            let second = ask(target);
            port.ask(dohyeon, second.clone()).await.unwrap();
            let answered = &peers.questions(dohyeon)[1];
            assert_eq!(answered.status, QuestionStatus::Answered);
            assert_eq!(
                answered.answer.as_ref().unwrap().text,
                "Merged this morning."
            );

            let private = AskTarget::Agent {
                member_id: dohyeon,
                agent_id: AgentId::generate(),
            };
            let error = port.ask(dohyeon, ask(private)).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::AgentNotShared);
            let elsewhere = AskTarget::Person {
                member_id: MemberId::generate(),
            };
            let error = port.ask(dohyeon, ask(elsewhere)).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidParams);
            let error = port.agents(MemberId::generate()).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn unsure_questions_are_routed_answered_refreshed_and_canceled() {
        let World {
            peers,
            dohyeon,
            api,
            web,
        } = world();
        let candidates = vec![RoutingCandidate {
            agent_id: api.agent_id,
            name: "api".into(),
            role: "Backend".into(),
            score: 0.55,
            reason: "mentions login".into(),
        }];
        peers
            .script_ask(dohyeon, AskReply::Unsure(candidates.clone()))
            .unwrap();
        block_on(async {
            let question = ask(AskTarget::Person { member_id: dohyeon });
            let id = question.question_id;
            peers.ask(dohyeon, question).await.unwrap();
            assert_eq!(peers.questions(dohyeon)[0].candidates, candidates);

            let route = |agent_id| PeerQuestionAction::Route {
                choice: RouteChoice::Agent { agent_id },
            };
            let error = peers
                .question_action(dohyeon, id, route(AgentId::generate()))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::AgentNotShared);
            let routed = peers
                .question_action(dohyeon, id, route(web.agent_id))
                .await
                .unwrap();
            assert_eq!(routed.status, QuestionStatus::Answering);
            assert_eq!(routed.agent_id, Some(web.agent_id));
            assert!(routed.candidates.is_empty());

            let answered = peers.answer(id, web.agent_id, "Done.").unwrap();
            assert_eq!(answered.status, QuestionStatus::Answered);
            let error = peers
                .question_action(dohyeon, id, PeerQuestionAction::Cancel)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
            let refreshed = peers
                .question_action(dohyeon, id, PeerQuestionAction::Refresh)
                .await
                .unwrap();
            assert_eq!(refreshed.status, QuestionStatus::Answering);
            let to_owner = PeerQuestionAction::Route {
                choice: RouteChoice::Owner,
            };
            let sent = peers.question_action(dohyeon, id, to_owner).await.unwrap();
            assert_eq!(sent.status, QuestionStatus::SentToOwner);
            let canceled = peers
                .question_action(dohyeon, id, PeerQuestionAction::Cancel)
                .await
                .unwrap();
            assert_eq!(canceled.status, QuestionStatus::Canceled);
            let error = peers
                .question_action(dohyeon, id, PeerQuestionAction::Refresh)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
            let error = peers
                .question_action(dohyeon, QuestionId::generate(), PeerQuestionAction::Cancel)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn offline_teammates_get_an_outbox_delivered_when_back() {
        let World {
            peers,
            dohyeon,
            api,
            ..
        } = world();
        let mut events = peers.subscribe();
        block_on(async {
            peers.set_online(dohyeon, false).unwrap();
            let PeerEvent::Presence { presence } = events.recv().await.unwrap() else {
                panic!("expected presence");
            };
            assert!(!presence.online);

            let question = ask(AskTarget::Person { member_id: dohyeon });
            for _ in 0..2 {
                let error = peers.ask(dohyeon, question.clone()).await.unwrap_err();
                assert_eq!(error.code, ErrorCode::PeerOffline);
            }
            let request = create(api.agent_id, "k1");
            let error = peers
                .create_request(dohyeon, request.clone())
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::PeerOffline);
            assert_eq!(peers.outbox().len(), 2);
            let error = peers.agents(dohyeon).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::PeerOffline);
            assert!(peers.questions(dohyeon).is_empty());

            let delivered = peers.set_online(dohyeon, true).unwrap();
            assert_eq!(delivered.len(), 2);
            assert!(delivered.iter().all(|(_, outcome)| outcome.is_ok()));
            assert!(peers.outbox().is_empty());
            assert_eq!(peers.questions(dohyeon)[0].id, question.question_id);
            assert_eq!(peers.requests(dohyeon)[0].id, request.request_id);
            let mut kinds = vec![];
            while let Ok(event) = events.try_recv() {
                kinds.push(match event {
                    PeerEvent::Presence { presence } => format!("presence {}", presence.online),
                    PeerEvent::QuestionUpdate { .. } => "question".into(),
                    PeerEvent::RequestUpdate { .. } => "request".into(),
                    other => format!("{other:?}"),
                });
            }
            assert_eq!(kinds, ["presence true", "question", "request"]);
        });
    }

    #[test]
    fn requests_are_idempotent_checked_and_cancelable_until_they_run() {
        let World {
            peers,
            dohyeon,
            api,
            web,
        } = world();
        block_on(async {
            let first = create(api.agent_id, "k1");
            let request = peers.create_request(dohyeon, first.clone()).await.unwrap();
            assert_eq!(request.status, RequestStatus::Queued);
            assert_eq!(request.requester, peers.me());
            assert_eq!(request.owner, dohyeon);
            let retry = PeerRequestCreate {
                request_id: RequestId::generate(),
                ..first
            };
            assert_eq!(peers.create_request(dohyeon, retry).await.unwrap(), request);

            let error = peers
                .create_request(dohyeon, create(web.agent_id, "k2"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RequestsNotAccepted);
            let error = peers
                .create_request(dohyeon, create(AgentId::generate(), "k3"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::AgentNotShared);

            let canceled = peers.cancel_request(dohyeon, request.id).await.unwrap();
            assert_eq!(canceled.status, RequestStatus::Canceled);
            assert_eq!(peers.request(dohyeon, request.id).await.unwrap(), canceled);
            let error = peers
                .set_request_status(request.id, RequestStatus::Running, None)
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);

            peers
                .set_arrival_status(dohyeon, RequestStatus::Received)
                .unwrap();
            let second = peers
                .create_request(dohyeon, create(api.agent_id, "k4"))
                .await
                .unwrap();
            assert_eq!(second.status, RequestStatus::Received);
            peers
                .set_request_status(second.id, RequestStatus::Running, None)
                .unwrap();
            let error = peers.cancel_request(dohyeon, second.id).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
            let done = peers
                .set_request_status(second.id, RequestStatus::Completed, Some("ok".into()))
                .unwrap();
            assert_eq!(done.result.as_deref(), Some("ok"));
            let error = peers
                .request(dohyeon, RequestId::generate())
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
        });
    }

    #[test]
    fn joining_records_the_whois_identity_once() {
        let peers = FakePeers::new(MemberId::generate());
        let creator = member("dohyeon", MemberRole::Creator);
        peers.set_roster(vec![creator.clone()]);
        let minsu = member("minsu", MemberRole::Member);
        peers.expect_join("100.66.100.62:7413", "fmn1-abcd", whois_of(&minsu));
        let join = |code: &str, login: &str| PeerJoin {
            code: code.into(),
            github_login: login.into(),
            display_name: login.into(),
        };
        block_on(async {
            let error = peers
                .join("100.64.0.1:7413", join("fmn1-abcd", "minsu"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::PeerOffline);
            let error = peers
                .join("100.66.100.62:7413", join("wrong", "minsu"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::NotFound);
            let error = peers
                .join("100.66.100.62:7413", join("fmn1-abcd", "dohyeon"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);

            let accepted = peers
                .join("100.66.100.62:7413", join("fmn1-abcd", "minsu"))
                .await
                .unwrap();
            assert_eq!(accepted.members.len(), 2);
            assert_eq!(accepted.member_id, peers.me());
            let joined = &accepted.members[1];
            assert_eq!(joined.id, accepted.member_id);
            assert_eq!(joined.tailscale_login, minsu.tailscale_login);
            assert_eq!(joined.tailscale_node, minsu.tailscale_node);
            assert_eq!(joined.role, MemberRole::Member);

            let error = peers
                .join("100.66.100.62:7413", join("fmn1-abcd", "jiwoo"))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Conflict);
        });
        assert_eq!(peers.calls().len(), 5);
    }

    #[test]
    fn scripted_failures_and_roster() {
        let World { peers, dohyeon, .. } = world();
        let creator = member("dohyeon", MemberRole::Creator);
        peers.set_roster(vec![creator.clone()]);
        peers
            .fail_next(dohyeon, ApiError::new(ErrorCode::NotMember, "removed"))
            .unwrap();
        block_on(async {
            let error = peers.roster(dohyeon).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember);
            assert_eq!(peers.roster(dohyeon).await.unwrap(), vec![creator]);
        });
        assert_eq!(
            peers.calls(),
            vec![PeersCall::Roster(dohyeon), PeersCall::Roster(dohyeon)]
        );
        let error = peers.set_agents(MemberId::generate(), vec![]).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }
}
