//! Owner: B. Repositories for team, sharing, questions, requests, approvals, and
//! rules, gathered in the [`CollabStore`] port.

use formation_proto::ask::Question;
use formation_proto::common::{
    AgentId, ApprovalId, MemberId, MessageId, QuestionId, RequestId, RuleId, SessionId, Timestamp,
};
use formation_proto::policy::{Approval, ApprovalStatus, Rule, ShareSettings};
use formation_proto::request::{QueueItem, WorkRequest};
use formation_proto::team::{Invitation, Member, TeamId};

use crate::ports::Result;

/// The team this Mac belongs to (one in the first version). On the creator's Mac it
/// is the authoritative roster's state; on a member's Mac, the copy last applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRecord {
    pub id: TeamId,
    /// Where the creator's Mac serves the roster and joins: its MagicDNS name or
    /// tailnet IP, from the invitation link (its own address on the creator's Mac).
    pub creator_host: String,
    pub creator_port: u16,
    /// The roster version held; only the creator's Mac increases it.
    pub roster_version: u64,
    /// The creator's open invitation, by code (creator's Mac only).
    pub open_invitation: Option<String>,
}

pub trait TeamRepo {
    /// This Mac's member, once a team exists.
    fn me(&self) -> Result<Option<MemberId>>;
    fn set_me(&self, id: MemberId) -> Result<()>;
    /// `None` until this Mac creates or joins a team, and again after it is removed.
    fn team(&self) -> Result<Option<TeamRecord>>;
    /// Replaces the record; `None` forgets the team.
    fn put_team(&self, team: Option<&TeamRecord>) -> Result<()>;
    /// Members not removed.
    fn members(&self) -> Result<Vec<Member>>;
    fn member(&self, id: MemberId) -> Result<Option<Member>>;
    /// Only members not removed; used on every peer request.
    fn member_by_tailscale(&self, login: &str) -> Result<Option<Member>>;
    /// Inserts or replaces a current member, clearing any earlier removal of the same
    /// ID (a Mac that forgot its team stores the roster again when it rejoins).
    fn put_member(&self, member: &Member) -> Result<()>;
    fn remove_member(&self, id: MemberId, at: Timestamp) -> Result<()>;
    fn put_invitation(&self, invitation: &Invitation) -> Result<()>;
    fn invitation_by_code(&self, code: &str) -> Result<Option<Invitation>>;
}

pub trait ShareRepo {
    /// [`ShareSettings::private`] when nothing is stored.
    fn share(&self, agent_id: AgentId) -> Result<ShareSettings>;
    fn shares(&self) -> Result<Vec<ShareSettings>>;
    fn put_share(&self, share: &ShareSettings) -> Result<()>;
}

pub trait QuestionRepo {
    fn question(&self, id: QuestionId) -> Result<Option<Question>>;
    fn questions(&self) -> Result<Vec<Question>>;
    fn put_question(&self, question: &Question) -> Result<()>;
    /// Removes this Mac's copy only.
    fn delete_question(&self, id: QuestionId) -> Result<()>;
    /// The forked session that answers a thread (owner's Mac only; never sent to peers).
    fn fork_session(&self, thread_id: QuestionId) -> Result<Option<SessionId>>;
    fn put_fork_session(&self, thread_id: QuestionId, session_id: &SessionId) -> Result<()>;
}

pub trait RequestRepo {
    /// Inserts the request, or returns the stored one when (requester,
    /// idempotency_key) exists. The flag is true when inserted. One transaction.
    fn insert_request(&self, request: &WorkRequest) -> Result<(WorkRequest, bool)>;
    fn request(&self, id: RequestId) -> Result<Option<WorkRequest>>;
    fn requests(&self) -> Result<Vec<WorkRequest>>;
    fn put_request(&self, request: &WorkRequest) -> Result<()>;
    /// The agent's queue in order, kept so a restart re-queues it.
    fn queue_items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>>;
    fn put_queue_item(&self, item: &QueueItem) -> Result<()>;
    fn remove_queue_item(&self, id: MessageId) -> Result<()>;
}

pub trait ApprovalRepo {
    fn approval(&self, id: ApprovalId) -> Result<Option<Approval>>;
    fn approvals(&self, status: Option<ApprovalStatus>) -> Result<Vec<Approval>>;
    fn put_approval(&self, approval: &Approval) -> Result<()>;
}

pub trait RuleRepo {
    fn rules(&self, agent_id: Option<AgentId>) -> Result<Vec<Rule>>;
    fn rule(&self, id: RuleId) -> Result<Option<Rule>>;
    fn put_rule(&self, rule: &Rule) -> Result<()>;
    /// Adds one use and returns the rule.
    fn record_rule_use(&self, id: RuleId) -> Result<Rule>;
}

/// B's repositories. B's SQLite store and in-memory store (TASK-042) implement all of them.
pub trait CollabStore:
    TeamRepo + ShareRepo + QuestionRepo + RequestRepo + ApprovalRepo + RuleRepo + Send + Sync
{
}

impl<T> CollabStore for T where
    T: TeamRepo + ShareRepo + QuestionRepo + RequestRepo + ApprovalRepo + RuleRepo + Send + Sync
{
}
