//! Owner: A. Runs agents' main sessions on a harness (Claude Code first).

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::agent::{Agent, AgentState, HarnessKind, HarnessStatus, StopMode};
use formation_proto::common::{AgentId, ItemId, SessionId};
use formation_proto::harness::{
    AgentStreamEvent, ConversationPageResult, SessionInfo, TurnOutcome,
};
use formation_proto::request::QueueItem;

use super::Result;

/// What a harness reports while it runs, for the UI. Receivers may lag and miss
/// events (`broadcast`); nothing that must not be lost travels here.
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    SessionStarted(SessionInfo),
    State {
        agent_id: AgentId,
        state: AgentState,
        reason: Option<String>,
        exit_code: Option<i32>,
    },
    Stream(AgentStreamEvent),
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    pub outcome: TurnOutcome,
    /// The agent's final reply text for the turn.
    pub result: Option<String>,
}

#[async_trait]
pub trait Harness: Send + Sync {
    fn kind(&self) -> HarnessKind;

    /// Installed, signed in, version.
    async fn status(&self) -> HarnessStatus;

    /// Starts the agent's main session in its folder, resuming `resume` when given.
    /// Does nothing when the session already runs.
    async fn start(&self, agent: &Agent, resume: Option<SessionId>) -> Result<SessionInfo>;

    /// Runs one turn with this queued message and returns when it ends. The executor
    /// calls it only when the agent is idle, one turn at a time per agent; queued
    /// messages stay in formationd until then (TASK-004: Claude Code gets each with
    /// `"priority":"later"`).
    async fn run_turn(&self, agent_id: AgentId, item: QueueItem) -> Result<TurnResult>;

    /// Delivers an owner message into the running turn, effective at the next tool
    /// boundary (TASK-004: a default-priority message). `harness_unsupported` for a
    /// harness that cannot steer.
    async fn steer(&self, agent_id: AgentId, item: QueueItem) -> Result<()>;

    /// Stops the running turn and keeps the session and process (TASK-004: the
    /// stream-json `control_request` interrupt).
    async fn interrupt(&self, agent_id: AgentId) -> Result<()>;

    async fn stop(&self, agent_id: AgentId, mode: StopMode) -> Result<()>;

    /// Session ID and transcript path, for the ask pipeline's fork (TASK-022).
    async fn session_info(&self, agent_id: AgentId) -> Result<Option<SessionInfo>>;

    /// The queued message whose turn is running, so the permission bridge and
    /// `from_agent` calls know the turn's caller and chain.
    async fn current_turn(&self, agent_id: AgentId) -> Option<QueueItem>;

    /// Items read from the session transcript, newest last.
    async fn conversation(
        &self,
        agent_id: AgentId,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult>;

    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent>;
}
