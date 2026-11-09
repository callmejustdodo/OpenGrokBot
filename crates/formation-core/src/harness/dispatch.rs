//! Runs agents on different harnesses side by side (TASK-041): routes each harness call
//! and command to the harness the agent's `formation.toml` names.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::broadcast;

use formation_proto::agent::{Agent, DetectedHarness, HarnessKind, HarnessStatus, StopMode};
use formation_proto::common::{AgentId, ApiError, ErrorCode, ItemId, RunId, SessionId};
use formation_proto::harness::{AgentStreamEvent, ConversationPageResult, SessionInfo};
use formation_proto::request::QueueItem;

use crate::ports::{
    CommandCall, CommandHandler, ForkResult, ForkRun, Harness, Result, RunInfo, Store, TurnResult,
};

struct Entry {
    harness: Arc<dyn Harness>,
    commands: Arc<dyn CommandHandler>,
}

/// One harness and command handler per `HarnessKind`, keyed by `Harness::kind`.
pub struct HarnessDispatcher {
    store: Arc<dyn Store>,
    entries: Vec<Entry>,
    stream: broadcast::Sender<AgentStreamEvent>,
}

impl HarnessDispatcher {
    /// The first entry answers `kind` and `status`. Must run inside a Tokio runtime:
    /// it spawns one task per harness that forwards its stream into `subscribe`.
    pub fn new(
        store: Arc<dyn Store>,
        harnesses: Vec<(Arc<dyn Harness>, Arc<dyn CommandHandler>)>,
    ) -> Self {
        assert!(!harnesses.is_empty(), "the dispatcher needs a harness");
        let (stream, _) = broadcast::channel(256);
        let entries = harnesses
            .into_iter()
            .map(|(harness, commands)| {
                let mut events = harness.subscribe();
                let merged = stream.clone();
                tokio::spawn(async move {
                    loop {
                        match events.recv().await {
                            Ok(event) => {
                                let _ = merged.send(event);
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });
                Entry { harness, commands }
            })
            .collect();
        Self {
            store,
            entries,
            stream,
        }
    }

    /// Every harness's status, in registration order.
    pub async fn statuses(&self) -> Vec<HarnessStatus> {
        let mut statuses = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            statuses.push(entry.harness.status().await);
        }
        statuses
    }

    fn entry(&self, agent_id: AgentId) -> Result<&Entry> {
        let agent = self
            .store
            .agent(agent_id)?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "agent not found"))?;
        self.entry_for(&agent)
    }

    fn entry_for(&self, agent: &Agent) -> Result<&Entry> {
        // A detected agent goes to the harness it runs on, which decides what it serves.
        let kind = match (agent.harness, agent.detected.as_ref().map(|d| d.harness)) {
            (HarnessKind::Detected, Some(DetectedHarness::ClaudeCode)) => HarnessKind::ClaudeCode,
            (HarnessKind::Detected, Some(DetectedHarness::Codex)) => HarnessKind::Codex,
            (kind, _) => kind,
        };
        self.entries
            .iter()
            .find(|entry| entry.harness.kind() == kind)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::HarnessUnsupported,
                    format!("no harness runs {kind:?} agents"),
                )
            })
    }
}

#[async_trait]
impl Harness for HarnessDispatcher {
    fn kind(&self) -> HarnessKind {
        self.entries[0].harness.kind()
    }
    async fn status(&self) -> HarnessStatus {
        self.entries[0].harness.status().await
    }
    async fn start(&self, agent: &Agent, resume: Option<SessionId>) -> Result<SessionInfo> {
        self.entry_for(agent)?.harness.start(agent, resume).await
    }
    async fn run_turn(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        item: QueueItem,
    ) -> Result<TurnResult> {
        self.entry(agent_id)?
            .harness
            .run_turn(agent_id, run_id, item)
            .await
    }
    async fn run_fork(&self, agent_id: AgentId, fork: ForkRun) -> Result<ForkResult> {
        self.entry(agent_id)?.harness.run_fork(agent_id, fork).await
    }
    async fn steer(&self, agent_id: AgentId, item: QueueItem) -> Result<()> {
        self.entry(agent_id)?.harness.steer(agent_id, item).await
    }
    async fn interrupt(&self, agent_id: AgentId) -> Result<()> {
        self.entry(agent_id)?.harness.interrupt(agent_id).await
    }
    async fn stop(&self, agent_id: AgentId, mode: StopMode) -> Result<()> {
        self.entry(agent_id)?.harness.stop(agent_id, mode).await
    }
    async fn session_info(&self, agent_id: AgentId) -> Result<Option<SessionInfo>> {
        self.entry(agent_id)?.harness.session_info(agent_id).await
    }
    async fn run(&self, run_id: RunId) -> Option<RunInfo> {
        for entry in &self.entries {
            if let Some(run) = entry.harness.run(run_id).await {
                return Some(run);
            }
        }
        None
    }
    async fn conversation(
        &self,
        agent_id: AgentId,
        session_id: Option<SessionId>,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult> {
        self.entry(agent_id)?
            .harness
            .conversation(agent_id, session_id, before, limit)
            .await
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.stream.subscribe()
    }
}

#[async_trait]
impl CommandHandler for HarnessDispatcher {
    fn methods(&self) -> Vec<&'static str> {
        let mut methods: Vec<&'static str> = Vec::new();
        for entry in &self.entries {
            for method in entry.commands.methods() {
                if !methods.contains(&method) {
                    methods.push(method);
                }
            }
        }
        methods
    }

    /// Every method these handlers serve names its agent in `agent_id`.
    async fn handle(&self, call: CommandCall) -> Result<Value> {
        let agent_id: AgentId = call
            .params
            .get("agent_id")
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidParams, "missing agent_id"))
            .and_then(|id| {
                serde_json::from_value(id)
                    .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
            })?;
        let entry = self.entry(agent_id)?;
        if !entry.commands.methods().contains(&call.method.as_str()) {
            return Err(ApiError::new(
                ErrorCode::HarnessUnsupported,
                format!("{} is not served for this agent's harness", call.method),
            ));
        }
        entry.commands.handle(call).await
    }
}
