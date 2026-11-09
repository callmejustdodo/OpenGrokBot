//! Small test doubles for this module's tests, until TASK-042's fakes merge.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::ask::{AskTarget, Question};
use formation_proto::common::{
    AgentId, ApiError, ApprovalId, ErrorCode, MemberId, MessageId, QuestionId, RequestId, RuleId,
    SessionId, Timestamp,
};
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerEvent, PeerJoin, PeerJoinAccepted, PeerQuestionAction,
    PeerRequestCreate, SharedAgentView,
};
use formation_proto::policy::{
    Approval, ApprovalStatus, Caller, PolicyVerdict, Rule, ShareSettings, ToolCall, Visibility,
};
use formation_proto::request::{QueueItem, WorkRequest};
use formation_proto::team::{Invitation, Member, WhoIs};

use super::{Requests, endpoints};
use crate::ports::{
    ApprovalRepo, Peers, Policy, QuestionRepo, RequestRepo, Result, RuleRepo, ShareRepo, TeamRepo,
    ToolContext,
};

const UNUSED: &str = "not used by the requests tests";

/// The parts of `CollabStore` the requests module uses, in memory. `insert_request`
/// is atomic on (requester, idempotency key), as the SQLite store's transaction is.
#[derive(Default)]
pub struct MemoryCollab {
    data: Mutex<Data>,
    /// Number of `insert_request` calls that inserted.
    pub inserts: AtomicUsize,
    /// Makes the next `put_queue_item` fail once.
    pub fail_queue_write: AtomicBool,
    /// Makes the next `put_request` fail once.
    pub fail_request_write: AtomicBool,
}

#[derive(Default)]
struct Data {
    me: Option<MemberId>,
    shares: Vec<ShareSettings>,
    requests: Vec<WorkRequest>,
    queue: Vec<QueueItem>,
}

impl MemoryCollab {
    pub fn new(me: MemberId) -> Self {
        let collab = Self::default();
        collab.data.lock().unwrap().me = Some(me);
        collab
    }

    pub fn share_agent(&self, agent_id: AgentId, accepts_requests: bool) {
        self.put_share(&ShareSettings {
            agent_id,
            shared: true,
            visibility: Visibility::AnswerExcerpts,
            accepts_requests,
        })
        .unwrap();
    }
}

impl TeamRepo for MemoryCollab {
    fn me(&self) -> Result<Option<MemberId>> {
        Ok(self.data.lock().unwrap().me)
    }
    fn set_me(&self, id: MemberId) -> Result<()> {
        self.data.lock().unwrap().me = Some(id);
        Ok(())
    }
    fn team(&self) -> Result<Option<crate::ports::TeamRecord>> {
        Ok(None)
    }
    fn put_team(&self, _team: Option<&crate::ports::TeamRecord>) -> Result<()> {
        Ok(())
    }
    fn members(&self) -> Result<Vec<Member>> {
        unimplemented!("{UNUSED}")
    }
    fn member(&self, _: MemberId) -> Result<Option<Member>> {
        unimplemented!("{UNUSED}")
    }
    fn member_by_tailscale(&self, _: &str) -> Result<Option<Member>> {
        unimplemented!("{UNUSED}")
    }
    fn put_member(&self, _: &Member) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn remove_member(&self, _: MemberId, _: Timestamp) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn put_invitation(&self, _: &Invitation) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn invitation_by_code(&self, _: &str) -> Result<Option<Invitation>> {
        unimplemented!("{UNUSED}")
    }
}

impl ShareRepo for MemoryCollab {
    fn share(&self, agent_id: AgentId) -> Result<ShareSettings> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .shares
            .iter()
            .find(|share| share.agent_id == agent_id)
            .cloned()
            .unwrap_or_else(|| ShareSettings::private(agent_id)))
    }
    fn shares(&self) -> Result<Vec<ShareSettings>> {
        Ok(self.data.lock().unwrap().shares.clone())
    }
    fn put_share(&self, share: &ShareSettings) -> Result<()> {
        let mut data = self.data.lock().unwrap();
        data.shares
            .retain(|stored| stored.agent_id != share.agent_id);
        data.shares.push(share.clone());
        Ok(())
    }
}

impl RequestRepo for MemoryCollab {
    fn insert_request(&self, request: &WorkRequest) -> Result<(WorkRequest, bool)> {
        let mut data = self.data.lock().unwrap();
        if let Some(stored) = data.requests.iter().find(|stored| {
            stored.requester == request.requester
                && stored.idempotency_key == request.idempotency_key
        }) {
            return Ok((stored.clone(), false));
        }
        if data.requests.iter().any(|stored| stored.id == request.id) {
            return Err(ApiError::new(ErrorCode::Conflict, "duplicate request ID"));
        }
        data.requests.push(request.clone());
        self.inserts.fetch_add(1, Ordering::SeqCst);
        Ok((request.clone(), true))
    }
    fn request(&self, id: RequestId) -> Result<Option<WorkRequest>> {
        Ok(self
            .data
            .lock()
            .unwrap()
            .requests
            .iter()
            .find(|request| request.id == id)
            .cloned())
    }
    fn requests(&self) -> Result<Vec<WorkRequest>> {
        Ok(self.data.lock().unwrap().requests.clone())
    }
    fn put_request(&self, request: &WorkRequest) -> Result<()> {
        if self.fail_request_write.swap(false, Ordering::SeqCst) {
            return Err(ApiError::new(ErrorCode::Internal, "disk full"));
        }
        let mut data = self.data.lock().unwrap();
        match data
            .requests
            .iter_mut()
            .find(|stored| stored.id == request.id)
        {
            Some(stored) => *stored = request.clone(),
            None => data.requests.push(request.clone()),
        }
        Ok(())
    }
    fn queue_items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>> {
        let mut items: Vec<QueueItem> = self
            .data
            .lock()
            .unwrap()
            .queue
            .iter()
            .filter(|item| item.agent_id == agent_id)
            .cloned()
            .collect();
        items.sort_by_key(|item| item.queued_at);
        Ok(items)
    }
    fn put_queue_item(&self, item: &QueueItem) -> Result<()> {
        if self.fail_queue_write.swap(false, Ordering::SeqCst) {
            return Err(ApiError::new(ErrorCode::Internal, "disk full"));
        }
        let mut data = self.data.lock().unwrap();
        match data.queue.iter_mut().find(|stored| stored.id == item.id) {
            Some(stored) => *stored = item.clone(),
            None => data.queue.push(item.clone()),
        }
        Ok(())
    }
    fn remove_queue_item(&self, id: MessageId) -> Result<()> {
        self.data.lock().unwrap().queue.retain(|item| item.id != id);
        Ok(())
    }
}

impl QuestionRepo for MemoryCollab {
    fn question(&self, _: QuestionId) -> Result<Option<Question>> {
        unimplemented!("{UNUSED}")
    }
    fn questions(&self) -> Result<Vec<Question>> {
        unimplemented!("{UNUSED}")
    }
    fn put_question(&self, _: &Question) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn delete_question(&self, _: QuestionId) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn fork_session(&self, _: QuestionId) -> Result<Option<SessionId>> {
        unimplemented!("{UNUSED}")
    }
    fn put_fork_session(&self, _: QuestionId, _: &SessionId) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
}

impl ApprovalRepo for MemoryCollab {
    fn approval(&self, _: ApprovalId) -> Result<Option<Approval>> {
        unimplemented!("{UNUSED}")
    }
    fn approvals(&self, _: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        unimplemented!("{UNUSED}")
    }
    fn put_approval(&self, _: &Approval) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
}

impl RuleRepo for MemoryCollab {
    fn rules(&self, _: Option<AgentId>) -> Result<Vec<Rule>> {
        unimplemented!("{UNUSED}")
    }
    fn rule(&self, _: RuleId) -> Result<Option<Rule>> {
        unimplemented!("{UNUSED}")
    }
    fn put_rule(&self, _: &Rule) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    fn record_rule_use(&self, _: RuleId) -> Result<Rule> {
        unimplemented!("{UNUSED}")
    }
}

/// Answers `check_request` with a scripted refusal, or allows; records the callers.
#[derive(Default)]
pub struct ScriptedPolicy {
    pub refusal: Mutex<Option<ApiError>>,
    pub checked: Mutex<Vec<(Caller, AgentId)>>,
}

#[async_trait]
impl Policy for ScriptedPolicy {
    async fn authenticate(&self, _: &WhoIs) -> Result<MemberId> {
        unimplemented!("{UNUSED}")
    }
    async fn visible_agents(&self, _: &Caller) -> Result<Vec<SharedAgentView>> {
        unimplemented!("{UNUSED}")
    }
    async fn check_ask(&self, _: &Caller, _: &AskTarget) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn check_request(&self, caller: &Caller, agent_id: AgentId) -> Result<()> {
        self.checked
            .lock()
            .unwrap()
            .push((caller.clone(), agent_id));
        match self.refusal.lock().unwrap().clone() {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }
    async fn visibility(&self, _: AgentId) -> Result<Visibility> {
        unimplemented!("{UNUSED}")
    }
    async fn evaluate(&self, _: &ToolContext, _: &ToolCall) -> Result<PolicyVerdict> {
        unimplemented!("{UNUSED}")
    }
}

/// Two Macs in one process: request calls from `requester` reach the owner's
/// [`Requests`] through its peer endpoint handlers, unless `offline` is set.
pub struct LoopbackPeers {
    pub requester: MemberId,
    pub owner: Arc<Requests>,
    pub offline: AtomicBool,
    pub sent: Mutex<Vec<PeerRequestCreate>>,
    /// Delivers the next request, stores the owner's copy in this store (as a
    /// concurrent retry would), then fails the send with the error.
    pub lose_reply: Mutex<Option<(Arc<MemoryCollab>, ApiError)>>,
    events: broadcast::Sender<PeerEvent>,
}

impl LoopbackPeers {
    pub fn new(requester: MemberId, owner: Arc<Requests>) -> Self {
        Self {
            requester,
            owner,
            offline: AtomicBool::new(false),
            sent: Mutex::new(Vec::new()),
            lose_reply: Mutex::new(None),
            events: broadcast::channel(16).0,
        }
    }

    fn reachable(&self) -> Result<()> {
        if self.offline.load(Ordering::SeqCst) {
            Err(ApiError::new(
                ErrorCode::PeerOffline,
                "queued in the outbox",
            ))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Peers for LoopbackPeers {
    async fn join(&self, _: &str, _: PeerJoin) -> Result<PeerJoinAccepted> {
        unimplemented!("{UNUSED}")
    }
    async fn roster(&self, _: MemberId) -> Result<Vec<Member>> {
        unimplemented!("{UNUSED}")
    }
    async fn agents(&self, _: MemberId) -> Result<Vec<SharedAgentView>> {
        unimplemented!("{UNUSED}")
    }
    async fn ask(&self, _: MemberId, _: PeerAsk) -> Result<PeerAskAccepted> {
        unimplemented!("{UNUSED}")
    }
    async fn question_action(
        &self,
        _: MemberId,
        _: QuestionId,
        _: PeerQuestionAction,
    ) -> Result<Question> {
        unimplemented!("{UNUSED}")
    }
    async fn create_request(
        &self,
        _member: MemberId,
        request: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        self.sent.lock().unwrap().push(request.clone());
        self.reachable()?;
        let lose_reply = self.lose_reply.lock().unwrap().take();
        let remote = endpoints::create(&self.owner, self.requester, request).await?;
        match lose_reply {
            Some((requester, failure)) => {
                requester.put_request(&remote)?;
                Err(failure)
            }
            None => Ok(remote),
        }
    }
    async fn request(&self, _member: MemberId, id: RequestId) -> Result<WorkRequest> {
        self.reachable()?;
        endpoints::get(&self.owner, self.requester, id)
    }
    async fn cancel_request(&self, _member: MemberId, id: RequestId) -> Result<WorkRequest> {
        self.reachable()?;
        endpoints::cancel(&self.owner, self.requester, id)
    }
    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.events.subscribe()
    }
}
