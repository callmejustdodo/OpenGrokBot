use crate::ports::*;
use formation_proto::{agent::*, ask::*, harness::*, peer::*, policy::*, request::*, team::*};


struct Stub { bus: Arc<EventBus>, fail: bool }

impl AgentRepo for Stub {
    fn agents(&self) -> Result<Vec<Agent>> { if self.fail { return Err(ApiError::new(ErrorCode::Internal, "store failed")); } self.bus.publish(event()); Ok(vec![agent()]) }
    fn agent(&self, _id: AgentId) -> Result<Option<Agent>> { unreachable!("unused port method") }
    fn agent_by_slug(&self, _slug: &str) -> Result<Option<Agent>> { unreachable!("unused port method") }
    fn insert_agent(&self, _agent: &Agent) -> Result<()> { unreachable!("unused port method") }
    fn update_agent(&self, _agent: &Agent) -> Result<()> { unreachable!("unused port method") }
    fn delete_agent(&self, _id: AgentId, _at: Timestamp) -> Result<()> { unreachable!("unused port method") }
}

impl SessionRepo for Stub {
    fn fork_session(&self, _run_id: RunId) -> Result<Option<SessionInfo>> { unreachable!("unused port method") }
    fn put_fork_session(&self, _run_id: RunId, _session: &SessionInfo) -> Result<()> { unreachable!("unused port method") }
    fn main_session(&self, _agent_id: AgentId) -> Result<Option<SessionInfo>> { Ok(None) }
    fn put_main_session(&self, _session: &SessionInfo) -> Result<()> { unreachable!("unused port method") }
}

impl ScheduleRepo for Stub {
    fn schedules(&self, _agent_id: Option<AgentId>) -> Result<Vec<Schedule>> { Ok(vec![]) }
    fn schedule(&self, _id: ScheduleId) -> Result<Option<Schedule>> { unreachable!("unused port method") }
    fn put_schedule(&self, _schedule: &Schedule) -> Result<()> { unreachable!("unused port method") }
    fn delete_schedule(&self, _id: ScheduleId) -> Result<()> { unreachable!("unused port method") }
    fn put_schedule_run(&self, _run: &ScheduleRun) -> Result<()> { unreachable!("unused port method") }
    fn schedule_runs( &self, _schedule_id: ScheduleId, _before: Option<Timestamp>, _limit: u32, ) -> Result<Vec<ScheduleRun>> { unreachable!("unused port method") }
}

impl AttentionRepo for Stub {
    fn open_attention(&self) -> Result<Vec<Attention>> { Ok(vec![]) }
    fn put_attention(&self, _item: &Attention) -> Result<()> { unreachable!("unused port method") }
    fn resolve_attention(&self, _target: &AttentionTarget, _at: Timestamp) -> Result<Vec<Attention>> { unreachable!("unused port method") }
}

impl ActivityRepo for Stub {
    fn record_activity(&self, _activity: &Activity) -> Result<()> { unreachable!("unused port method") }
    fn activity( &self, _agent_id: Option<AgentId>, _member_id: Option<MemberId>, _before: Option<Timestamp>, _limit: u32, ) -> Result<Vec<Activity>> { unreachable!("unused port method") }
    fn delete_activity(&self, _ids: &[ActivityId]) -> Result<()> { unreachable!("unused port method") }
}

impl SettingsRepo for Stub {
    fn settings(&self) -> Result<Settings> { Ok(Settings { language: Some(formation_proto::snapshot::Language::Ko), ..Settings::default() }) }
    fn put_settings(&self, _settings: &Settings) -> Result<()> { unreachable!("unused port method") }
}

impl TeamRepo for Stub {
    fn me(&self) -> Result<Option<MemberId>> { Ok(Some(member_id())) }
    fn set_me(&self, _id: MemberId) -> Result<()> { unreachable!("unused port method") }
    fn members(&self) -> Result<Vec<Member>> { Ok(vec![member(member_id()), member(teammate_id())]) }
    fn member(&self, _id: MemberId) -> Result<Option<Member>> { unreachable!("unused port method") }
    fn member_by_tailscale(&self, _login: &str) -> Result<Option<Member>> { unreachable!("unused port method") }
    fn put_member(&self, _member: &Member) -> Result<()> { unreachable!("unused port method") }
    fn remove_member(&self, _id: MemberId, _at: Timestamp) -> Result<()> { unreachable!("unused port method") }
    fn put_invitation(&self, _invitation: &Invitation) -> Result<()> { unreachable!("unused port method") }
    fn invitation_by_code(&self, _code: &str) -> Result<Option<Invitation>> { unreachable!("unused port method") }
}

impl ShareRepo for Stub {
    fn share(&self, _agent_id: AgentId) -> Result<ShareSettings> { unreachable!("unused port method") }
    fn shares(&self) -> Result<Vec<ShareSettings>> { Ok(vec![ShareSettings::private(agent_id())]) }
    fn put_share(&self, _share: &ShareSettings) -> Result<()> { unreachable!("unused port method") }
}

impl QuestionRepo for Stub {
    fn question(&self, _id: QuestionId) -> Result<Option<Question>> { unreachable!("unused port method") }
    fn questions(&self) -> Result<Vec<Question>> { Ok(vec![]) }
    fn put_question(&self, _question: &Question) -> Result<()> { unreachable!("unused port method") }
    fn delete_question(&self, _id: QuestionId) -> Result<()> { unreachable!("unused port method") }
    fn fork_session(&self, _thread_id: QuestionId) -> Result<Option<SessionId>> { unreachable!("unused port method") }
    fn put_fork_session(&self, _thread_id: QuestionId, _session_id: &SessionId) -> Result<()> { unreachable!("unused port method") }
}

impl RequestRepo for Stub {
    fn insert_request(&self, _request: &WorkRequest) -> Result<(WorkRequest, bool)> { unreachable!("unused port method") }
    fn request(&self, _id: RequestId) -> Result<Option<WorkRequest>> { unreachable!("unused port method") }
    fn requests(&self) -> Result<Vec<WorkRequest>> { Ok(vec![]) }
    fn put_request(&self, _request: &WorkRequest) -> Result<()> { unreachable!("unused port method") }
    fn queue_items(&self, _agent_id: AgentId) -> Result<Vec<QueueItem>> { Ok(vec![]) }
    fn put_queue_item(&self, _item: &QueueItem) -> Result<()> { unreachable!("unused port method") }
    fn remove_queue_item(&self, _id: MessageId) -> Result<()> { unreachable!("unused port method") }
}

impl ApprovalRepo for Stub {
    fn approval(&self, _id: ApprovalId) -> Result<Option<Approval>> { unreachable!("unused port method") }
    fn approvals(&self, _status: Option<ApprovalStatus>) -> Result<Vec<Approval>> { Ok(vec![]) }
    fn put_approval(&self, _approval: &Approval) -> Result<()> { unreachable!("unused port method") }
}

impl RuleRepo for Stub {
    fn rules(&self, _agent_id: Option<AgentId>) -> Result<Vec<Rule>> { Ok(vec![]) }
    fn rule(&self, _id: RuleId) -> Result<Option<Rule>> { unreachable!("unused port method") }
    fn put_rule(&self, _rule: &Rule) -> Result<()> { unreachable!("unused port method") }
    fn record_rule_use(&self, _id: RuleId) -> Result<Rule> { unreachable!("unused port method") }
}

#[async_trait]
impl Harness for Stub {
    fn kind(&self) -> HarnessKind { HarnessKind::ClaudeCode }
    async fn status(&self) -> HarnessStatus { HarnessStatus { harness: HarnessKind::ClaudeCode, installed: true, signed_in: Some(true), version: None, path: None, problem: None } }
    async fn start(&self, _agent: &Agent, _resume: Option<SessionId>) -> Result<SessionInfo> { unreachable!("unused port method") }
    async fn run_turn(&self, _agent_id: AgentId, _run_id: RunId, _item: QueueItem) -> Result<TurnResult> { unreachable!("unused port method") }
    async fn run_fork(&self, _agent_id: AgentId, _fork: ForkRun) -> Result<ForkResult> { unreachable!("unused port method") }
    async fn steer(&self, _agent_id: AgentId, _item: QueueItem) -> Result<()> { unreachable!("unused port method") }
    async fn interrupt(&self, _agent_id: AgentId) -> Result<()> { unreachable!("unused port method") }
    async fn stop(&self, _agent_id: AgentId, _mode: StopMode) -> Result<()> { unreachable!("unused port method") }
    async fn session_info(&self, _agent_id: AgentId) -> Result<Option<SessionInfo>> { unreachable!("unused port method") }
    async fn run(&self, _run_id: RunId) -> Option<RunInfo> { unreachable!("unused port method") }
    async fn conversation( &self, _agent_id: AgentId, _session_id: Option<SessionId>, _before: Option<ItemId>, _limit: u32, ) -> Result<ConversationPageResult> { assert_eq!(_agent_id, agent_id()); assert_eq!(_session_id, None); assert_eq!(_before, None); assert!(_limit > 0); Ok(ConversationPageResult { items: vec![], has_more: true, stream_seq: 7 }) }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> { unreachable!("unused port method") }
}

#[async_trait]
impl Peers for Stub {
    async fn join(&self, _creator_address: &str, _join: PeerJoin) -> Result<PeerJoinAccepted> { unreachable!("unused port method") }
    async fn roster(&self, _member: MemberId) -> Result<Vec<Member>> { unreachable!("unused port method") }
    async fn agents(&self, _member: MemberId) -> Result<Vec<SharedAgentView>> { assert_eq!(_member, teammate_id()); Ok(vec![]) }
    async fn ask(&self, _member: MemberId, _ask: PeerAsk) -> Result<PeerAskAccepted> { unreachable!("unused port method") }
    async fn question_action( &self, _member: MemberId, _question_id: QuestionId, _action: PeerQuestionAction, ) -> Result<Question> { unreachable!("unused port method") }
    async fn create_request( &self, _member: MemberId, _request: PeerRequestCreate, ) -> Result<WorkRequest> { unreachable!("unused port method") }
    async fn request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> { unreachable!("unused port method") }
    async fn cancel_request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> { unreachable!("unused port method") }
    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> { unreachable!("unused port method") }
}

