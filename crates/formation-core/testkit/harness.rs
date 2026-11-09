use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

use crate::ports::{ForkResult, ForkRun, ForkTools, Harness, Result, RunInfo, TurnResult};
use async_trait::async_trait;
use formation_proto::{
    agent::{Agent, HarnessKind, HarnessStatus, StopMode},
    common::{AgentId, ApiError, ErrorCode, ItemId, RunId, SessionId},
    harness::{AgentStreamEvent, ConversationPageResult, SessionInfo},
    request::QueueItem,
};
use tokio::sync::broadcast;

/// A page returned for an exact scripted pagination request.
pub struct ScriptedPage {
    pub session_id: Option<SessionId>,
    pub before: Option<ItemId>,
    pub limit: u32,
    pub page: ConversationPageResult,
}

pub struct ScriptedAgent {
    pub session: SessionInfo,
    pub turns: VecDeque<TurnResult>,
    pub forks: VecDeque<ForkResult>,
    pub pages: Vec<ScriptedPage>,
}

impl ScriptedAgent {
    pub fn new(session: SessionInfo, turns: impl IntoIterator<Item = TurnResult>) -> Self {
        Self {
            session,
            turns: turns.into_iter().collect(),
            forks: VecDeque::new(),
            pages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessCall {
    RunTurn {
        agent_id: AgentId,
        run_id: RunId,
        item: QueueItem,
    },
    RunFork {
        agent_id: AgentId,
        run_id: RunId,
        from: SessionId,
        item: QueueItem,
        tools: ForkTools,
    },
    Steer {
        agent_id: AgentId,
        item: QueueItem,
    },
    Interrupt {
        agent_id: AgentId,
    },
    Stop {
        agent_id: AgentId,
        mode: StopMode,
    },
}

pub struct FakeHarness {
    kind: HarnessKind,
    status: HarnessStatus,
    agents: Mutex<HashMap<AgentId, ScriptedAgent>>,
    calls: Mutex<Vec<HarnessCall>>,
    runs: Mutex<HashMap<RunId, RunInfo>>,
    stream: broadcast::Sender<AgentStreamEvent>,
}

fn not_found() -> ApiError {
    ApiError::new(ErrorCode::NotFound, "no scripted agent or response")
}

impl FakeHarness {
    pub fn new(
        kind: HarnessKind,
        status: HarnessStatus,
        agents: impl IntoIterator<Item = ScriptedAgent>,
    ) -> Self {
        Self {
            kind,
            status,
            agents: Mutex::new(
                agents
                    .into_iter()
                    .map(|a| (a.session.agent_id, a))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            runs: Mutex::new(HashMap::new()),
            stream: broadcast::channel(64).0,
        }
    }
    pub fn calls(&self) -> Vec<HarnessCall> {
        self.calls.lock().unwrap().clone()
    }
    pub fn sender(&self) -> broadcast::Sender<AgentStreamEvent> {
        self.stream.clone()
    }
    /// Scripts an agent created after the harness, such as one made through
    /// `agent_create`.
    pub fn add_agent(&self, agent: ScriptedAgent) {
        self.agents
            .lock()
            .unwrap()
            .insert(agent.session.agent_id, agent);
    }
    pub fn set_run(&self, run: RunInfo) {
        self.runs.lock().unwrap().insert(run.run_id, run);
    }
    fn record(&self, agent_id: AgentId, call: HarnessCall) -> Result<()> {
        if !self.agents.lock().unwrap().contains_key(&agent_id) {
            return Err(not_found());
        }
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

#[async_trait]
impl Harness for FakeHarness {
    fn kind(&self) -> HarnessKind {
        self.kind
    }
    async fn status(&self) -> HarnessStatus {
        self.status.clone()
    }
    async fn start(&self, agent: &Agent, _resume: Option<SessionId>) -> Result<SessionInfo> {
        self.session_info(agent.id).await?.ok_or_else(not_found)
    }
    async fn run_turn(
        &self,
        agent_id: AgentId,
        run_id: RunId,
        item: QueueItem,
    ) -> Result<TurnResult> {
        self.record(
            agent_id,
            HarnessCall::RunTurn {
                agent_id,
                run_id,
                item,
            },
        )?;
        self.agents
            .lock()
            .unwrap()
            .get_mut(&agent_id)
            .ok_or_else(not_found)?
            .turns
            .pop_front()
            .ok_or_else(not_found)
    }
    async fn run_fork(&self, agent_id: AgentId, fork: ForkRun) -> Result<ForkResult> {
        self.record(
            agent_id,
            HarnessCall::RunFork {
                agent_id,
                run_id: fork.run_id,
                from: fork.from,
                item: fork.item,
                tools: fork.tools,
            },
        )?;
        self.agents
            .lock()
            .unwrap()
            .get_mut(&agent_id)
            .ok_or_else(not_found)?
            .forks
            .pop_front()
            .ok_or_else(not_found)
    }
    async fn steer(&self, agent_id: AgentId, item: QueueItem) -> Result<()> {
        self.record(agent_id, HarnessCall::Steer { agent_id, item })
    }
    async fn interrupt(&self, agent_id: AgentId) -> Result<()> {
        self.record(agent_id, HarnessCall::Interrupt { agent_id })
    }
    async fn stop(&self, agent_id: AgentId, mode: StopMode) -> Result<()> {
        self.record(agent_id, HarnessCall::Stop { agent_id, mode })
    }
    async fn session_info(&self, agent_id: AgentId) -> Result<Option<SessionInfo>> {
        Ok(Some(
            self.agents
                .lock()
                .unwrap()
                .get(&agent_id)
                .ok_or_else(not_found)?
                .session
                .clone(),
        ))
    }
    async fn run(&self, run_id: RunId) -> Option<RunInfo> {
        self.runs.lock().unwrap().get(&run_id).cloned()
    }
    async fn conversation(
        &self,
        agent_id: AgentId,
        session_id: Option<SessionId>,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult> {
        let agents = self.agents.lock().unwrap();
        let agent = agents.get(&agent_id).ok_or_else(not_found)?;
        let session_id = session_id.as_ref().unwrap_or(&agent.session.session_id);
        agent
            .pages
            .iter()
            .find(|p| {
                p.session_id.as_ref().unwrap_or(&agent.session.session_id) == session_id
                    && p.before == before
                    && p.limit == limit
            })
            .map(|p| p.page.clone())
            .ok_or_else(not_found)
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.stream.subscribe()
    }
}
