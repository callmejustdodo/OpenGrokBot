//! Owner: A. Local storage for the agent runtime, split into repositories. B's
//! repositories form a separate port, [`CollabStore`], in `store/collab.rs`. Both
//! use the same SQLite file, each with its own migration range. Repositories are
//! synchronous (SQLite); call them from blocking-safe code.

use std::ops::Range;

use formation_proto::agent::{Activity, Agent, Attention, AttentionTarget, Schedule, ScheduleRun};
use formation_proto::common::{ActivityId, AgentId, MemberId, ScheduleId, Timestamp};
use formation_proto::harness::SessionInfo;
use formation_proto::snapshot::Settings;

use super::Result;

mod collab;

pub use collab::{
    ApprovalRepo, CollabStore, QuestionRepo, RequestRepo, RuleRepo, ShareRepo, TeamRepo,
};

/// Migration versions A may use.
pub const MIGRATIONS_A: Range<u32> = 1000..2000;
/// Migration versions B may use.
pub const MIGRATIONS_B: Range<u32> = 2000..3000;
const _: () = assert!(
    MIGRATIONS_A.end <= MIGRATIONS_B.start,
    "migration ranges overlap"
);

/// One schema step. The store applies them in version order at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

pub trait AgentRepo {
    /// Agents not deleted.
    fn agents(&self) -> Result<Vec<Agent>>;
    fn agent(&self, id: AgentId) -> Result<Option<Agent>>;
    fn agent_by_slug(&self, slug: &str) -> Result<Option<Agent>>;
    /// `conflict` when the slug is taken.
    fn insert_agent(&self, agent: &Agent) -> Result<()>;
    fn update_agent(&self, agent: &Agent) -> Result<()>;
    fn delete_agent(&self, id: AgentId, at: Timestamp) -> Result<()>;
}

pub trait SessionRepo {
    fn main_session(&self, agent_id: AgentId) -> Result<Option<SessionInfo>>;
    fn put_main_session(&self, session: &SessionInfo) -> Result<()>;
}

pub trait ScheduleRepo {
    fn schedules(&self, agent_id: Option<AgentId>) -> Result<Vec<Schedule>>;
    fn schedule(&self, id: ScheduleId) -> Result<Option<Schedule>>;
    fn put_schedule(&self, schedule: &Schedule) -> Result<()>;
    fn delete_schedule(&self, id: ScheduleId) -> Result<()>;
    fn put_schedule_run(&self, run: &ScheduleRun) -> Result<()>;
    /// Runs for the schedule, newest first.
    fn schedule_runs(
        &self,
        schedule_id: ScheduleId,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<ScheduleRun>>;
}

pub trait AttentionRepo {
    /// Unresolved items, oldest first.
    fn open_attention(&self) -> Result<Vec<Attention>>;
    fn put_attention(&self, item: &Attention) -> Result<()>;
    /// Resolves every open item for the target; returns the resolved items.
    fn resolve_attention(&self, target: &AttentionTarget, at: Timestamp) -> Result<Vec<Attention>>;
}

pub trait ActivityRepo {
    fn record_activity(&self, activity: &Activity) -> Result<()>;
    /// Filters by agent and member before applying the limit for `history_list`.
    fn activity(
        &self,
        agent_id: Option<AgentId>,
        member_id: Option<MemberId>,
        before: Option<Timestamp>,
        limit: u32,
    ) -> Result<Vec<Activity>>;
    fn delete_activity(&self, ids: &[ActivityId]) -> Result<()>;
}

pub trait SettingsRepo {
    fn settings(&self) -> Result<Settings>;
    fn put_settings(&self, settings: &Settings) -> Result<()>;
}

/// A's repositories. The SQLite store and the in-memory store (TASK-003) implement
/// all of them.
pub trait Store:
    AgentRepo + SessionRepo + ScheduleRepo + AttentionRepo + ActivityRepo + SettingsRepo + Send + Sync
{
}

impl<T> Store for T where
    T: AgentRepo
        + SessionRepo
        + ScheduleRepo
        + AttentionRepo
        + ActivityRepo
        + SettingsRepo
        + Send
        + Sync
{
}
