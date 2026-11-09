use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

use crate::ports::{Harness, Result, TurnResult};
use async_trait::async_trait;
use formation_proto::{
    agent::{Agent, HarnessKind, HarnessStatus, StopMode},
    common::{AgentId, ApiError, ErrorCode, ItemId, SessionId},
    harness::{AgentStreamEvent, ConversationPageResult, SessionInfo},
    request::QueueItem,
};
use tokio::sync::broadcast;

/// A page returned for an exact scripted pagination request.
pub struct ScriptedPage {
    pub before: Option<ItemId>,
    pub limit: u32,
    pub page: ConversationPageResult,
}

pub struct ScriptedAgent {
    pub session: SessionInfo,
    pub turns: VecDeque<TurnResult>,
    pub current_turn: Option<QueueItem>,
    pub pages: Vec<ScriptedPage>,
}

impl ScriptedAgent {
    pub fn new(session: SessionInfo, turns: impl IntoIterator<Item = TurnResult>) -> Self {
        Self {
            session,
            turns: turns.into_iter().collect(),
            current_turn: None,
            pages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessCall {
    Steer { agent_id: AgentId, item: QueueItem },
    Interrupt { agent_id: AgentId },
    Stop { agent_id: AgentId, mode: StopMode },
}

pub struct FakeHarness {
    kind: HarnessKind,
    status: HarnessStatus,
    agents: Mutex<HashMap<AgentId, ScriptedAgent>>,
    calls: Mutex<Vec<HarnessCall>>,
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
            stream: broadcast::channel(64).0,
        }
    }
    pub fn calls(&self) -> Vec<HarnessCall> {
        self.calls.lock().unwrap().clone()
    }
    pub fn sender(&self) -> broadcast::Sender<AgentStreamEvent> {
        self.stream.clone()
    }
    pub fn set_current_turn(&self, agent_id: AgentId, item: Option<QueueItem>) -> Result<()> {
        self.agents
            .lock()
            .unwrap()
            .get_mut(&agent_id)
            .ok_or_else(not_found)?
            .current_turn = item;
        Ok(())
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
    async fn run_turn(&self, agent_id: AgentId, _item: QueueItem) -> Result<TurnResult> {
        self.agents
            .lock()
            .unwrap()
            .get_mut(&agent_id)
            .ok_or_else(not_found)?
            .turns
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
    async fn current_turn(&self, agent_id: AgentId) -> Option<QueueItem> {
        self.agents
            .lock()
            .unwrap()
            .get(&agent_id)
            .and_then(|a| a.current_turn.clone())
    }
    async fn conversation(
        &self,
        agent_id: AgentId,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult> {
        self.agents
            .lock()
            .unwrap()
            .get(&agent_id)
            .ok_or_else(not_found)?
            .pages
            .iter()
            .find(|p| p.before == before && p.limit == limit)
            .map(|p| p.page.clone())
            .ok_or_else(not_found)
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.stream.subscribe()
    }
}
