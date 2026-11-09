//! Owner: A. Runs agents' main sessions and their forks on a harness (Claude Code first).

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::agent::{Agent, HarnessKind, HarnessStatus, StopMode};
use formation_proto::common::{AgentId, ItemId, RunId, SessionId};
use formation_proto::harness::{
    AgentStreamEvent, ConversationPageResult, SessionInfo, TurnOutcome,
};
use formation_proto::request::QueueItem;

use super::Result;

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    pub outcome: TurnOutcome,
    /// The agent's final reply text for the turn.
    pub result: Option<String>,
}

/// A run's identity and queued item, used to find its caller and chain.
#[derive(Debug, Clone)]
pub struct RunInfo {
    pub run_id: RunId,
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub item: QueueItem,
}

/// A fork of an existing session.
#[derive(Debug, Clone)]
pub struct ForkRun {
    pub run_id: RunId,
    pub from: SessionId,
    pub item: QueueItem,
    pub tools: ForkTools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkTools {
    /// `--tools ""`, for the ask pipeline's answer.
    None,
    /// Tools go through the permission gate for a teammate's work request.
    Gated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForkResult {
    pub session: SessionInfo,
    pub result: TurnResult,
}

#[async_trait]
pub trait Harness: Send + Sync {
    fn kind(&self) -> HarnessKind;

    /// Installed, signed in, version.
    async fn status(&self) -> HarnessStatus;

    /// Starts the agent's main session in its folder, resuming `resume` when given.
    /// Does nothing when the session already runs. Detected agents (sessions started
    /// outside Formation) are read and fork only (#14); returns `harness_unsupported`.
    async fn start(&self, agent: &Agent, resume: Option<SessionId>) -> Result<SessionInfo>;

    /// Runs one turn with this queued message and returns when it ends. The executor
    /// calls it only when the agent is idle, one turn at a time per agent; queued
    /// messages stay in formationd until then (TASK-004: Claude Code gets each with
    /// `"priority":"later"`).
    /// Only `QueueSource::Owner` and `QueueSource::Schedule` run here;
    /// `QueueSource::Request` runs with `run_fork`. The executor assigns `run_id`.
    async fn run_turn(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        item: QueueItem,
    ) -> Result<TurnResult>;

    /// Starts with `--resume <from> --fork-session` and never writes the main
    /// session (#14). File isolation and how results reach the owner are open (#14).
    async fn run_fork(&self, agent_id: AgentId, fork: ForkRun) -> Result<ForkResult>;

    /// Delivers an owner message into the running turn, effective at the next tool
    /// boundary (TASK-004: a default-priority message). `harness_unsupported` for a
    /// harness that cannot steer or for a detected agent, which is read and fork only
    /// (#14). Returns `conflict` when no turn is running.
    async fn steer(&self, agent_id: AgentId, item: QueueItem) -> Result<()>;

    /// Stops the running turn and keeps the session and process (TASK-004: the
    /// stream-json `control_request` interrupt).
    async fn interrupt(&self, agent_id: AgentId) -> Result<()>;

    async fn stop(&self, agent_id: AgentId, mode: StopMode) -> Result<()>;

    /// The main session that forks start from.
    async fn session_info(&self, agent_id: AgentId) -> Result<Option<SessionInfo>>;

    /// The permission gate and `from_run` calls use this to find the run's caller
    /// and chain.
    async fn run(&self, run_id: RunId) -> Option<RunInfo>;

    /// Items read from the session transcript, newest last. `None` selects the main session.
    async fn conversation(
        &self,
        agent_id: AgentId,
        session_id: Option<SessionId>,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult>;

    /// Stream events for the UI; receivers may lag and miss events (`broadcast`).
    /// The adapter publishes numbered `agent.state` events through `EventSink` so
    /// clients detect gaps, and stores sessions with `SessionRepo`.
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent>;
}
