use async_trait::async_trait;
use formation_core::ports::*;
use formation_proto::{ask::*, common::*, peer::*, policy::*, request::*, team::*};
use tokio::sync::broadcast;

// TASK-042 replaces this temporary no-team adapter with B's adapters.
pub struct NoTeam {
    events: broadcast::Sender<PeerEvent>,
}

impl NoTeam {
    pub fn new() -> Self {
        Self {
            events: broadcast::channel(16).0,
        }
    }
}

fn unavailable<T>() -> Result<T> {
    Err(ApiError::new(
        ErrorCode::PeerOffline,
        "no team configured (TASK-042)",
    ))
}

impl TeamRepo for NoTeam {
    fn me(&self) -> Result<Option<MemberId>> {
        Ok(None)
    }
    fn set_me(&self, _id: MemberId) -> Result<()> {
        unavailable()
    }
    fn members(&self) -> Result<Vec<Member>> {
        Ok(vec![])
    }
    fn member(&self, _id: MemberId) -> Result<Option<Member>> {
        Ok(None)
    }
    fn member_by_tailscale(&self, _login: &str) -> Result<Option<Member>> {
        Ok(None)
    }
    fn put_member(&self, _member: &Member) -> Result<()> {
        unavailable()
    }
    fn remove_member(&self, _id: MemberId, _at: Timestamp) -> Result<()> {
        unavailable()
    }
    fn put_invitation(&self, _invitation: &Invitation) -> Result<()> {
        unavailable()
    }
    fn invitation_by_code(&self, _code: &str) -> Result<Option<Invitation>> {
        Ok(None)
    }
}
impl ShareRepo for NoTeam {
    fn share(&self, agent_id: AgentId) -> Result<ShareSettings> {
        Ok(ShareSettings::private(agent_id))
    }
    fn shares(&self) -> Result<Vec<ShareSettings>> {
        Ok(vec![])
    }
    fn put_share(&self, _share: &ShareSettings) -> Result<()> {
        unavailable()
    }
}
impl QuestionRepo for NoTeam {
    fn question(&self, _id: QuestionId) -> Result<Option<Question>> {
        Ok(None)
    }
    fn questions(&self) -> Result<Vec<Question>> {
        Ok(vec![])
    }
    fn put_question(&self, _question: &Question) -> Result<()> {
        unavailable()
    }
    fn delete_question(&self, _id: QuestionId) -> Result<()> {
        unavailable()
    }
    fn fork_session(&self, _thread_id: QuestionId) -> Result<Option<SessionId>> {
        Ok(None)
    }
    fn put_fork_session(&self, _thread_id: QuestionId, _session_id: &SessionId) -> Result<()> {
        unavailable()
    }
}
impl RequestRepo for NoTeam {
    fn insert_request(&self, _request: &WorkRequest) -> Result<(WorkRequest, bool)> {
        unavailable()
    }
    fn request(&self, _id: RequestId) -> Result<Option<WorkRequest>> {
        Ok(None)
    }
    fn requests(&self) -> Result<Vec<WorkRequest>> {
        Ok(vec![])
    }
    fn put_request(&self, _request: &WorkRequest) -> Result<()> {
        unavailable()
    }
    fn queue_items(&self, _agent_id: AgentId) -> Result<Vec<QueueItem>> {
        Ok(vec![])
    }
    fn put_queue_item(&self, _item: &QueueItem) -> Result<()> {
        unavailable()
    }
    fn remove_queue_item(&self, _id: MessageId) -> Result<()> {
        unavailable()
    }
}
impl ApprovalRepo for NoTeam {
    fn approval(&self, _id: ApprovalId) -> Result<Option<Approval>> {
        Ok(None)
    }
    fn approvals(&self, _status: Option<ApprovalStatus>) -> Result<Vec<Approval>> {
        Ok(vec![])
    }
    fn put_approval(&self, _approval: &Approval) -> Result<()> {
        unavailable()
    }
}
impl RuleRepo for NoTeam {
    fn rules(&self, _agent_id: Option<AgentId>) -> Result<Vec<Rule>> {
        Ok(vec![])
    }
    fn rule(&self, _id: RuleId) -> Result<Option<Rule>> {
        Ok(None)
    }
    fn put_rule(&self, _rule: &Rule) -> Result<()> {
        unavailable()
    }
    fn record_rule_use(&self, _id: RuleId) -> Result<Rule> {
        unavailable()
    }
}
#[async_trait]
impl Peers for NoTeam {
    async fn join(&self, _creator_address: &str, _join: PeerJoin) -> Result<PeerJoinAccepted> {
        unavailable()
    }
    async fn roster(&self, _member: MemberId) -> Result<Vec<Member>> {
        Ok(vec![])
    }
    async fn agents(&self, _member: MemberId) -> Result<Vec<SharedAgentView>> {
        Ok(vec![])
    }
    async fn ask(&self, _member: MemberId, _ask: PeerAsk) -> Result<PeerAskAccepted> {
        unavailable()
    }
    async fn question_action(
        &self,
        _member: MemberId,
        _question_id: QuestionId,
        _action: PeerQuestionAction,
    ) -> Result<Question> {
        unavailable()
    }
    async fn create_request(
        &self,
        _member: MemberId,
        _request: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        unavailable()
    }
    async fn request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> {
        unavailable()
    }
    async fn cancel_request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> {
        unavailable()
    }
    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.events.subscribe()
    }
}
