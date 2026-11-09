//! Long-lived `codex app-server` processes, one per running agent. Each actor owns
//! the child's stdin and Formation's queue for the agent: the server does not queue
//! turns, so a message waits here until the previous `turn/completed`.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use formation_proto::{
    agent::*,
    common::*,
    events::Event,
    harness::*,
    policy::{Caller, PermissionPrompt, PermissionSource, PermissionVerdict, ToolCall},
    request::{QueueItem, QueueSource},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{broadcast, mpsc, oneshot},
    time::Instant,
};

use crate::ports::{
    CommandCall, CommandHandler, EventSink, ForkResult, ForkRun, Harness, PermissionGate,
    PermissionRequest, Result, RunInfo, Store, TurnResult,
};

#[derive(Clone, Debug)]
pub struct CodexConfig {
    pub binary: PathBuf,
    pub stalled_after: Duration,
    /// After "stop now", how long a turn may run before the process is killed.
    pub stop_timeout: Duration,
    /// How long `initialize` and `thread/start` (or `thread/resume`) may take.
    pub start_timeout: Duration,
    /// Child-only environment, including fake-codex fixture selection in tests.
    pub env: HashMap<String, String>,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            binary: "codex".into(),
            stalled_after: Duration::from_secs(600),
            stop_timeout: Duration::from_secs(10),
            start_timeout: Duration::from_secs(30),
            env: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct CodexSupervisor {
    inner: Arc<Inner>,
}
struct Inner {
    config: CodexConfig,
    store: Arc<dyn Store>,
    sink: Arc<dyn EventSink>,
    gate: Option<Arc<dyn PermissionGate>>,
    agents: Mutex<HashMap<AgentId, Running>>,
    streams: broadcast::Sender<AgentStreamEvent>,
    /// Held across the handshake, so concurrent starts cannot spawn twice.
    starting: tokio::sync::Mutex<()>,
}
struct Running {
    tx: mpsc::UnboundedSender<Control>,
    state: AgentState,
    session: SessionInfo,
    seq: u64,
    /// Completed items streamed by this supervisor, oldest first. Earlier history
    /// lives only in Codex's rollout file, which is not read yet.
    items: Vec<ConversationItem>,
    run: Option<RunInfo>,
    finished: bool,
}
struct Pending {
    run_id: RunId,
    item: QueueItem,
    done: oneshot::Sender<Result<TurnResult>>,
}
struct Active {
    pending: Pending,
    /// Known once `turn/start` answers.
    turn_id: Option<String>,
    /// The last agent message of the turn.
    reply: Option<String>,
}
enum Control {
    Turn(Pending),
    Steer(QueueItem, oneshot::Sender<Result<()>>),
    SteerQueued(MessageId, oneshot::Sender<Result<()>>),
    Edit(MessageId, String, oneshot::Sender<Result<QueueItem>>),
    Withdraw(MessageId, oneshot::Sender<Result<()>>),
    Interrupt(oneshot::Sender<Result<()>>),
    Stop(StopMode, oneshot::Sender<Result<()>>),
}
/// What a client request's response completes.
enum Waiting {
    TurnStart,
    Steer(oneshot::Sender<Result<()>>),
    Interrupt(oneshot::Sender<Result<()>>),
}

fn error(code: ErrorCode, message: impl ToString) -> ApiError {
    ApiError::new(code, message.to_string())
}
fn internal(message: impl ToString) -> ApiError {
    error(ErrorCode::Internal, message)
}
fn now() -> Timestamp {
    SystemTime::now().into()
}
fn text_input(text: &str) -> Value {
    json!([{"type": "text", "text": text, "text_elements": []}])
}
fn item_id(item: &ConversationItem) -> &ItemId {
    match item {
        ConversationItem::UserMessage { id, .. }
        | ConversationItem::AssistantText { id, .. }
        | ConversationItem::Step { id, .. }
        | ConversationItem::Approval { id, .. }
        | ConversationItem::Request { id, .. }
        | ConversationItem::Notice { id, .. } => id,
    }
}

/// A Codex thread item as the conversation shows it; `None` for items Formation
/// does not show (user messages are shown at dispatch, reasoning is hidden).
fn conversation_item(item: &Value, at: Timestamp) -> Option<ConversationItem> {
    let id = ItemId(item["id"].as_str()?.to_owned());
    let text = |key: &str| item[key].as_str().unwrap_or_default().to_owned();
    let status = match item["status"].as_str() {
        Some("inProgress") => StepStatus::Running,
        Some("failed") => StepStatus::Failed,
        Some("declined") => StepStatus::Denied,
        _ => StepStatus::Done,
    };
    let step = |kind, tool_name: String, target: String, detail: Option<String>| {
        Some(ConversationItem::Step {
            id: id.clone(),
            at,
            step: Step {
                kind,
                tool_name,
                target,
                detail,
                status,
            },
        })
    };
    match item["type"].as_str()? {
        "agentMessage" => Some(ConversationItem::AssistantText {
            id: id.clone(),
            at,
            text: text("text"),
        }),
        "commandExecution" => step(
            StepKind::Command,
            "Bash".into(),
            text("command"),
            item["aggregatedOutput"].as_str().map(str::to_owned),
        ),
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let field = |key: &str| {
                changes
                    .iter()
                    .filter_map(|c| c[key].as_str())
                    .collect::<Vec<_>>()
            };
            let diff = field("diff").join("\n");
            step(
                StepKind::Edit,
                "Edit".into(),
                field("path").join(", "),
                (!diff.is_empty()).then_some(diff),
            )
        }
        "webSearch" => step(StepKind::Web, "WebSearch".into(), text("query"), None),
        "mcpToolCall" => step(StepKind::Tool, text("tool"), text("server"), None),
        _ => None,
    }
}

/// The tool call the permission gate sees for a Codex approval request.
fn tool_call(method: &str, params: &Value, started: &HashMap<String, Value>) -> ToolCall {
    let item_id = params["itemId"].as_str().unwrap_or_default().to_owned();
    let (tool_name, input) = if method == "item/commandExecution/requestApproval" {
        (
            "Bash",
            json!({"command": params["command"], "cwd": params["cwd"]}),
        )
    } else {
        // The changes arrive on the item, not on the request.
        let changes = started
            .get(&item_id)
            .map_or(Value::Null, |item| item["changes"].clone());
        (
            "Edit",
            json!({"changes": changes, "reason": params["reason"]}),
        )
    };
    ToolCall {
        tool_use_id: Some(item_id),
        tool_name: tool_name.into(),
        input,
    }
}

async fn write(stdin: &mut ChildStdin, message: Value) -> Result<()> {
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .map_err(internal)?;
    stdin.flush().await.map_err(internal)
}

/// Sends a request during the handshake and waits for its response, skipping
/// notifications.
async fn call(
    stdin: &mut ChildStdin,
    lines: &mut Lines<BufReader<ChildStdout>>,
    id: u64,
    method: &str,
    params: Value,
) -> Result<Value> {
    write(
        stdin,
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )
    .await?;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(internal)?
            .ok_or_else(|| error(ErrorCode::HarnessUnavailable, "Codex exited during start"))?;
        let msg: Value = serde_json::from_str(&line).map_err(internal)?;
        if msg.get("method").is_none() && msg["id"] == id {
            if let Some(e) = msg.get("error") {
                return Err(error(
                    ErrorCode::HarnessUnavailable,
                    format!("Codex {method} failed: {e}"),
                ));
            }
            return Ok(msg["result"].clone());
        }
    }
}

impl CodexSupervisor {
    /// Without a gate, every approval request is declined.
    pub fn new(
        config: CodexConfig,
        store: Arc<dyn Store>,
        sink: Arc<dyn EventSink>,
        gate: Option<Arc<dyn PermissionGate>>,
    ) -> Self {
        let (streams, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                config,
                store,
                sink,
                gate,
                agents: Mutex::new(HashMap::new()),
                streams,
                starting: tokio::sync::Mutex::new(()),
            }),
        }
    }
    fn sender(&self, id: AgentId) -> Result<mpsc::UnboundedSender<Control>> {
        self.inner
            .agents
            .lock()
            .unwrap()
            .get(&id)
            .filter(|r| !r.tx.is_closed())
            .map(|r| r.tx.clone())
            .ok_or_else(|| error(ErrorCode::Conflict, "agent is not running"))
    }
    fn enqueue(
        &self,
        id: AgentId,
        run_id: RunId,
        item: QueueItem,
    ) -> Result<oneshot::Receiver<Result<TurnResult>>> {
        if item.agent_id != id {
            return Err(error(
                ErrorCode::InvalidParams,
                "queue item belongs to another agent",
            ));
        }
        if matches!(item.source, QueueSource::Request { .. }) {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "requests require a fork",
            ));
        }
        let (done, rx) = oneshot::channel();
        self.sender(id)?
            .send(Control::Turn(Pending { run_id, item, done }))
            .map_err(internal)?;
        Ok(rx)
    }
    fn current_state(&self, id: AgentId) -> AgentState {
        self.inner.agents.lock().unwrap()[&id].state
    }
    fn state(
        &self,
        id: AgentId,
        state: AgentState,
        reason: Option<String>,
        exit_code: Option<i32>,
    ) {
        let mut agents = self.inner.agents.lock().unwrap();
        let r = agents.get_mut(&id).expect("registered supervisor");
        r.state = state;
        self.inner.sink.publish(Event::AgentState(AgentStateEvent {
            agent_id: id,
            state,
            reason,
            exit_code,
        }));
    }
    fn stream(&self, id: AgentId, delta: StreamDelta) {
        let mut agents = self.inner.agents.lock().unwrap();
        let r = agents.get_mut(&id).expect("registered supervisor");
        r.seq += 1;
        if let StreamDelta::ItemCompleted { item } = &delta {
            r.items.push(item.clone());
        }
        let event = AgentStreamEvent {
            agent_id: id,
            session_id: r.session.session_id.clone(),
            stream_seq: r.seq,
            delta,
        };
        self.inner.sink.publish(Event::AgentStream(event.clone()));
        let _ = self.inner.streams.send(event);
    }
    fn publish_queue(&self, id: AgentId, queue: &VecDeque<Pending>) {
        self.inner.sink.publish(Event::AgentQueue(AgentQueueEvent {
            agent_id: id,
            items: queue.iter().map(|p| p.item.clone()).collect(),
        }));
    }
    /// Publishes the queue and stores the owner's waiting messages for a restart.
    fn queue(&self, id: AgentId, queue: &VecDeque<Pending>) -> Result<()> {
        self.publish_queue(id, queue);
        let owner: Vec<_> = queue
            .iter()
            .filter(|p| p.item.source == QueueSource::Owner)
            .map(|p| p.item.clone())
            .collect();
        self.inner.store.put_owner_queue(id, &owner)
    }
    fn agent(&self, id: AgentId) -> Result<Agent> {
        let mut agent = self
            .inner
            .store
            .agent(id)?
            .ok_or_else(|| error(ErrorCode::NotFound, "agent not found"))?;
        if let Some(r) = self.inner.agents.lock().unwrap().get(&id) {
            agent.state = r.state;
        }
        Ok(agent)
    }
    fn codex_agent(&self, id: AgentId) -> Result<Agent> {
        let agent = self.agent(id)?;
        if agent.harness != HarnessKind::Codex || agent.detected.is_some() {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only Formation Codex agents run here",
            ));
        }
        Ok(agent)
    }
    fn user_message(&self, id: AgentId, item: &QueueItem) {
        self.stream(
            id,
            StreamDelta::ItemCompleted {
                item: ConversationItem::UserMessage {
                    id: ItemId(item.id.to_string()),
                    at: now(),
                    author: match item.source {
                        QueueSource::Schedule { schedule_id } => Author::Schedule { schedule_id },
                        _ => Author::Owner,
                    },
                    text: item.text.clone(),
                },
            },
        );
    }
    fn thread_id(&self, id: AgentId) -> String {
        self.inner.agents.lock().unwrap()[&id]
            .session
            .session_id
            .0
            .clone()
    }
}

#[async_trait]
impl Harness for CodexSupervisor {
    fn kind(&self) -> HarnessKind {
        HarnessKind::Codex
    }
    async fn status(&self) -> HarnessStatus {
        // Sign-in is not checked: Formation cannot tell it without starting Codex.
        let binary = self.inner.config.binary.clone();
        let output = Command::new(&binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output();
        let version = match tokio::time::timeout(Duration::from_secs(10), output).await {
            Ok(Ok(out)) if out.status.success() => {
                Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
            }
            _ => None,
        };
        HarnessStatus {
            harness: self.kind(),
            installed: version.is_some(),
            signed_in: None,
            problem: version.is_none().then(|| "Install Codex CLI".into()),
            version,
            path: Some(binary),
        }
    }
    async fn start(&self, agent: &Agent, resume: Option<SessionId>) -> Result<SessionInfo> {
        if agent.harness != HarnessKind::Codex || agent.detected.is_some() {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only Formation Codex agents can start",
            ));
        }
        let _starting = self.inner.starting.lock().await;
        if let Some(r) = self
            .inner
            .agents
            .lock()
            .unwrap()
            .get(&agent.id)
            .filter(|r| !r.finished)
        {
            return Ok(r.session.clone());
        }
        let stored = self.inner.store.main_session(agent.id)?;
        let restored = self.inner.store.owner_queue(agent.id)?;
        let resume = resume.or_else(|| stored.as_ref().map(|s| s.session_id.clone()));
        let config = &self.inner.config;
        let mut child = Command::new(&config.binary)
            .arg("app-server")
            .current_dir(&agent.folder_path)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| error(ErrorCode::HarnessUnavailable, e))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let settings = json!({
            "cwd": agent.folder_path,
            "approvalPolicy": "untrusted",
            "approvalsReviewer": "user",
            "sandbox": "workspace-write",
        });
        let handshake = async {
            call(
                &mut stdin,
                &mut lines,
                1,
                "initialize",
                json!({
                    "clientInfo": {"name": "formation", "title": "Formation", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi": false},
                }),
            )
            .await?;
            write(
                &mut stdin,
                json!({"jsonrpc": "2.0", "method": "initialized"}),
            )
            .await?;
            match &resume {
                Some(sid) => {
                    let mut params = settings.clone();
                    params["threadId"] = json!(sid.0);
                    call(&mut stdin, &mut lines, 2, "thread/resume", params).await
                }
                None => call(&mut stdin, &mut lines, 2, "thread/start", settings.clone()).await,
            }
        };
        let result = match tokio::time::timeout(config.start_timeout, handshake).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(error(
                    ErrorCode::HarnessUnavailable,
                    "Codex did not start in time",
                ));
            }
        };
        let thread = &result["thread"];
        let thread_id = thread["id"]
            .as_str()
            .ok_or_else(|| internal("Codex returned no thread ID"))?;
        if resume.as_ref().is_some_and(|sid| sid.0 != thread_id) {
            return Err(internal("Codex resumed a different thread"));
        }
        let session = SessionInfo {
            agent_id: agent.id,
            session_id: SessionId(thread_id.to_owned()),
            transcript_path: thread["path"]
                .as_str()
                .map(PathBuf::from)
                .or_else(|| stored.as_ref().map(|s| s.transcript_path.clone()))
                .unwrap_or_default(),
            cwd: agent.folder_path.clone(),
            started_at: now(),
            last_activity_at: stored.and_then(|s| s.last_activity_at),
        };
        self.inner.store.put_main_session(&session)?;
        let (tx, rx) = mpsc::unbounded_channel();
        // Messages the owner queued before a restart run first, in their order.
        for item in restored {
            let (done, _) = oneshot::channel();
            tx.send(Control::Turn(Pending {
                run_id: RunId::generate(),
                item,
                done,
            }))
            .map_err(internal)?;
        }
        {
            let mut agents = self.inner.agents.lock().unwrap();
            let (seq, items) = agents
                .remove(&agent.id)
                .map_or((0, Vec::new()), |r| (r.seq, r.items));
            agents.insert(
                agent.id,
                Running {
                    tx,
                    state: AgentState::Idle,
                    session: session.clone(),
                    seq,
                    items,
                    run: None,
                    finished: false,
                },
            );
        }
        self.state(agent.id, AgentState::Idle, None, None);
        let supervisor = self.clone();
        let id = agent.id;
        tokio::spawn(async move {
            supervisor.supervise(id, child, stdin, lines, rx).await;
        });
        Ok(session)
    }
    async fn run_turn(&self, id: AgentId, run_id: RunId, item: QueueItem) -> Result<TurnResult> {
        self.enqueue(id, run_id, item)?.await.map_err(internal)?
    }
    async fn run_fork(&self, _id: AgentId, _fork: ForkRun) -> Result<ForkResult> {
        Err(error(
            ErrorCode::HarnessUnsupported,
            "Codex agents cannot fork yet",
        ))
    }
    async fn steer(&self, id: AgentId, item: QueueItem) -> Result<()> {
        self.codex_agent(id)?;
        if item.agent_id != id {
            return Err(error(
                ErrorCode::InvalidParams,
                "queue item belongs to another agent",
            ));
        }
        if item.source != QueueSource::Owner {
            return Err(error(ErrorCode::Denied, "only owner messages can steer"));
        }
        let (done, rx) = oneshot::channel();
        self.sender(id)?
            .send(Control::Steer(item, done))
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    async fn interrupt(&self, id: AgentId) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender(id)?
            .send(Control::Interrupt(tx))
            .map_err(internal)?;
        rx.await.map_err(internal)?
    }
    async fn stop(&self, id: AgentId, mode: StopMode) -> Result<()> {
        let tx = match self.sender(id) {
            Ok(tx) => tx,
            Err(_) if self.inner.store.main_session(id)?.is_some() => return Ok(()),
            Err(e) => return Err(e),
        };
        let (done, rx) = oneshot::channel();
        tx.send(Control::Stop(mode, done)).map_err(internal)?;
        rx.await.map_err(internal)?
    }
    async fn session_info(&self, id: AgentId) -> Result<Option<SessionInfo>> {
        self.inner.store.main_session(id)
    }
    async fn run(&self, id: RunId) -> Option<RunInfo> {
        self.inner
            .agents
            .lock()
            .unwrap()
            .values()
            .find_map(|r| r.run.as_ref().filter(|run| run.run_id == id).cloned())
    }
    async fn conversation(
        &self,
        id: AgentId,
        session_id: Option<SessionId>,
        before: Option<ItemId>,
        limit: u32,
    ) -> Result<ConversationPageResult> {
        if limit == 0 {
            return Err(error(ErrorCode::InvalidParams, "limit must be positive"));
        }
        let session = self
            .inner
            .store
            .main_session(id)?
            .ok_or_else(|| error(ErrorCode::NotFound, "no main session"))?;
        if session_id.is_some_and(|sid| sid != session.session_id) {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only the main session is supported",
            ));
        }
        let (mut items, stream_seq) = self
            .inner
            .agents
            .lock()
            .unwrap()
            .get(&id)
            .map_or((Vec::new(), 0), |r| (r.items.clone(), r.seq));
        let end = match before {
            Some(before) => items
                .iter()
                .position(|item| item_id(item) == &before)
                .ok_or_else(|| error(ErrorCode::NotFound, "conversation cursor not found"))?,
            None => items.len(),
        };
        let start = end.saturating_sub(limit as usize);
        items.truncate(end);
        let items = items.drain(start..).collect();
        Ok(ConversationPageResult {
            items,
            has_more: start > 0,
            stream_seq,
        })
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.inner.streams.subscribe()
    }
}

fn queue_error(message_id: MessageId, started: &HashSet<MessageId>) -> ApiError {
    if started.contains(&message_id) {
        error(ErrorCode::Conflict, "message already started")
    } else {
        error(ErrorCode::NotFound, "queued message not found")
    }
}

impl CodexSupervisor {
    /// Sends `turn/steer` for the running turn as request `request_id`.
    async fn send_steer(
        &self,
        id: AgentId,
        stdin: &mut ChildStdin,
        request_id: u64,
        turn_id: String,
        item: &QueueItem,
    ) -> Result<()> {
        write(
            stdin,
            json!({"jsonrpc": "2.0", "id": request_id, "method": "turn/steer", "params": {
                "threadId": self.thread_id(id),
                "input": text_input(&item.text),
                "expectedTurnId": turn_id,
            }}),
        )
        .await?;
        self.user_message(id, item);
        Ok(())
    }

    /// Asks the gate in its own task, so the actor keeps serving interrupts and stops
    /// while the owner decides.
    fn ask_gate(
        &self,
        id: AgentId,
        request_id: Value,
        call: ToolCall,
        run_id: RunId,
        answers: mpsc::UnboundedSender<(Value, &'static str)>,
    ) {
        let request = PermissionRequest {
            prompt: PermissionPrompt {
                agent_id: id,
                run_id,
                session_id: Some(SessionId(self.thread_id(id))),
                source: PermissionSource::PermissionTool,
                call,
            },
            caller: Caller::Owner,
            request_id: None,
        };
        let gate = self.inner.gate.clone();
        tokio::spawn(async move {
            let verdict = match gate {
                Some(gate) => gate.decide(request).await.ok(),
                None => None,
            };
            let decision = match verdict {
                Some(PermissionVerdict::Allow { .. }) => "accept",
                _ => "decline",
            };
            let _ = answers.send((request_id, decision));
        });
    }

    async fn supervise(
        &self,
        id: AgentId,
        mut child: Child,
        mut stdin: ChildStdin,
        mut lines: Lines<BufReader<ChildStdout>>,
        mut rx: mpsc::UnboundedReceiver<Control>,
    ) {
        let mut queue = VecDeque::<Pending>::new();
        let mut active: Option<Active> = None;
        let mut steered = Vec::<Pending>::new();
        let mut started = HashSet::new();
        // Requests 1 and 2 were the handshake.
        let mut next_id = 2u64;
        let mut waiting = HashMap::<u64, Waiting>::new();
        let mut items_started = HashMap::<String, Value>::new();
        let (answers_tx, mut answers) = mpsc::unbounded_channel::<(Value, &'static str)>();
        let mut stopping = false;
        let mut stops = Vec::new();
        let mut interrupted = false;
        let mut stalled = false;
        let mut last = Instant::now();
        let mut exit_code = None;
        let mut kill_at: Option<Instant> = None;
        let failure = loop {
            if active.is_none() {
                if stopping {
                    break None;
                }
                if let Some(p) = queue.pop_front() {
                    started.insert(p.item.id);
                    if let Err(e) = self.queue(id, &queue) {
                        break Some(e);
                    }
                    let thread_id = self.thread_id(id);
                    self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run = Some(RunInfo {
                        run_id: p.run_id,
                        agent_id: id,
                        session_id: SessionId(thread_id.clone()),
                        item: p.item.clone(),
                    });
                    self.state(id, AgentState::Working, None, None);
                    self.stream(
                        id,
                        StreamDelta::TurnStarted {
                            message_id: p.item.id,
                        },
                    );
                    self.user_message(id, &p.item);
                    next_id += 1;
                    waiting.insert(next_id, Waiting::TurnStart);
                    let sent = write(
                        &mut stdin,
                        json!({"jsonrpc": "2.0", "id": next_id, "method": "turn/start", "params": {
                            "threadId": thread_id,
                            "input": text_input(&p.item.text),
                        }}),
                    )
                    .await;
                    active = Some(Active {
                        pending: p,
                        turn_id: None,
                        reply: None,
                    });
                    if let Err(e) = sent {
                        break Some(e);
                    }
                    interrupted = false;
                    stalled = false;
                    last = Instant::now();
                }
            }
            tokio::select! {
                control = rx.recv() => match control {
                    Some(Control::Turn(p)) => {
                        if stopping {
                            let _ = p.done.send(Err(error(ErrorCode::Conflict, "agent is stopping")));
                        } else {
                            queue.push_back(p);
                            if let Err(e) = self.queue(id, &queue) {
                                break Some(e);
                            }
                        }
                    }
                    Some(Control::Steer(item, done)) => {
                        if stopping {
                            let _ = done.send(Err(error(ErrorCode::Conflict, "agent is stopping")));
                            continue;
                        }
                        let Some(turn_id) = active.as_ref().and_then(|a| a.turn_id.clone()) else {
                            let _ = done.send(Err(error(ErrorCode::Conflict, "agent has no running turn")));
                            continue;
                        };
                        started.insert(item.id);
                        next_id += 1;
                        waiting.insert(next_id, Waiting::Steer(done));
                        if let Err(e) = self.send_steer(id, &mut stdin, next_id, turn_id, &item).await {
                            break Some(e);
                        }
                    }
                    Some(Control::SteerQueued(message_id, done)) => {
                        let index = queue.iter().position(|p| p.item.id == message_id);
                        let turn_id = active.as_ref().and_then(|a| a.turn_id.clone());
                        let result = match (turn_id, index) {
                            _ if stopping => Err(error(ErrorCode::Conflict, "agent is stopping")),
                            (None, _) => Err(error(ErrorCode::Conflict, "agent has no running turn")),
                            (Some(_), None) => Err(queue_error(message_id, &started)),
                            (Some(_), Some(index)) if queue[index].item.source != QueueSource::Owner => {
                                Err(error(ErrorCode::Denied, "only owner messages can steer"))
                            }
                            (Some(turn_id), Some(index)) => Ok((index, turn_id)),
                        };
                        match result {
                            Err(e) => {
                                let _ = done.send(Err(e));
                            }
                            Ok((index, turn_id)) => {
                                let p = queue.remove(index).unwrap();
                                started.insert(message_id);
                                next_id += 1;
                                waiting.insert(next_id, Waiting::Steer(done));
                                if let Err(e) = self.send_steer(id, &mut stdin, next_id, turn_id, &p.item).await {
                                    break Some(e);
                                }
                                steered.push(p);
                                if let Err(e) = self.queue(id, &queue) {
                                    break Some(e);
                                }
                            }
                        }
                    }
                    Some(Control::Edit(message_id, text, done)) => {
                        let result = if let Some(p) = queue.iter_mut().find(|p| p.item.id == message_id) {
                            p.item.text = text;
                            let item = p.item.clone();
                            self.queue(id, &queue).map(|()| item)
                        } else {
                            Err(queue_error(message_id, &started))
                        };
                        let _ = done.send(result);
                    }
                    Some(Control::Withdraw(message_id, done)) => {
                        let result = if let Some(index) = queue.iter().position(|p| p.item.id == message_id) {
                            let p = queue.remove(index).unwrap();
                            let _ = p.done.send(Err(error(ErrorCode::Conflict, "message withdrawn")));
                            self.queue(id, &queue)
                        } else {
                            Err(queue_error(message_id, &started))
                        };
                        let _ = done.send(result);
                    }
                    Some(Control::Interrupt(done)) => {
                        match active.as_ref().and_then(|a| a.turn_id.clone()) {
                            None => {
                                let _ = done.send(Err(error(ErrorCode::Conflict, "no running turn")));
                            }
                            Some(turn_id) => {
                                interrupted = true;
                                next_id += 1;
                                waiting.insert(next_id, Waiting::Interrupt(done));
                                let message = json!({"jsonrpc": "2.0", "id": next_id, "method": "turn/interrupt", "params": {"threadId": self.thread_id(id), "turnId": turn_id}});
                                if let Err(e) = write(&mut stdin, message).await {
                                    break Some(e);
                                }
                            }
                        }
                    }
                    Some(Control::Stop(mode, done)) => {
                        stopping = true;
                        stops.push(done);
                        if mode == StopMode::Now && !interrupted
                            && let Some(turn_id) = active.as_ref().and_then(|a| a.turn_id.clone())
                        {
                            interrupted = true;
                            next_id += 1;
                            let message = json!({"jsonrpc": "2.0", "id": next_id, "method": "turn/interrupt", "params": {"threadId": self.thread_id(id), "turnId": turn_id}});
                            if let Err(e) = write(&mut stdin, message).await {
                                break Some(e);
                            }
                        }
                        // A turn that ignores the interrupt is killed after the timeout.
                        if mode == StopMode::Now && kill_at.is_none() && active.is_some() {
                            kill_at = Some(Instant::now() + self.inner.config.stop_timeout);
                        }
                    }
                    None => break None,
                },
                Some((request_id, decision)) = answers.recv() => {
                    if let Err(e) = write(&mut stdin, json!({"jsonrpc": "2.0", "id": request_id, "result": {"decision": decision}})).await {
                        break Some(e);
                    }
                }
                line = lines.next_line() => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            let status = child.wait().await;
                            exit_code = status.as_ref().ok().and_then(|s| s.code());
                            break Some(error(ErrorCode::HarnessUnavailable, format!("Codex exited: {status:?}")));
                        }
                        Err(e) => break Some(internal(e)),
                    };
                    last = Instant::now();
                    if stalled {
                        stalled = false;
                        self.state(id, AgentState::Working, None, None);
                    }
                    let msg: Value = match serde_json::from_str(&line) {
                        Ok(msg) => msg,
                        Err(e) => break Some(internal(e)),
                    };
                    let activity = {
                        let mut agents = self.inner.agents.lock().unwrap();
                        let r = agents.get_mut(&id).unwrap();
                        r.session.last_activity_at = Some(now());
                        r.session.clone()
                    };
                    if let Err(e) = self.inner.store.put_main_session(&activity) {
                        break Some(e);
                    }
                    let params = &msg["params"];
                    match (msg["method"].as_str(), msg.get("id")) {
                        // A response to one of our requests.
                        (None, Some(response_id)) => {
                            let outcome = match msg.get("error") {
                                Some(e) => Err(error(ErrorCode::HarnessUnavailable, format!("Codex: {e}"))),
                                None => Ok(()),
                            };
                            match response_id.as_u64().and_then(|n| waiting.remove(&n)) {
                                Some(Waiting::TurnStart) => match outcome {
                                    Ok(()) => {
                                        if let Some(a) = active.as_mut() {
                                            a.turn_id = msg["result"]["turn"]["id"].as_str().map(str::to_owned);
                                        }
                                    }
                                    Err(e) => {
                                        if let Some(a) = active.take() {
                                            self.end_turn(id, a, TurnOutcome::Failed, Some(e.to_string()), &mut steered);
                                        }
                                    }
                                },
                                Some(Waiting::Steer(done)) | Some(Waiting::Interrupt(done)) => {
                                    let _ = done.send(outcome);
                                }
                                None => {}
                            }
                        }
                        // A server request: approvals go to the gate, the rest are refused.
                        (Some(method), Some(request_id)) => {
                            if method == "item/commandExecution/requestApproval" || method == "item/fileChange/requestApproval" {
                                let run_id = active.as_ref().map_or_else(RunId::generate, |a| a.pending.run_id);
                                let call = tool_call(method, params, &items_started);
                                self.ask_gate(id, request_id.clone(), call, run_id, answers_tx.clone());
                            } else {
                                let reply = json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": format!("Formation does not handle {method}")}});
                                if let Err(e) = write(&mut stdin, reply).await {
                                    break Some(e);
                                }
                            }
                        }
                        (Some("item/started"), None) => {
                            let item = &params["item"];
                            if let Some(item_id) = item["id"].as_str() {
                                items_started.insert(item_id.to_owned(), item.clone());
                            }
                            if let Some(item) = conversation_item(item, now()) {
                                self.stream(id, StreamDelta::ItemStarted { item });
                            }
                        }
                        (Some("item/agentMessage/delta"), None) => {
                            if let (Some(item_id), Some(text)) = (params["itemId"].as_str(), params["delta"].as_str()) {
                                self.stream(id, StreamDelta::TextDelta { item_id: ItemId(item_id.to_owned()), text: text.to_owned() });
                            }
                        }
                        (Some("item/completed"), None) => {
                            let item = &params["item"];
                            if let Some(item_id) = item["id"].as_str() {
                                items_started.remove(item_id);
                            }
                            if item["type"] == "agentMessage"
                                && let Some(a) = active.as_mut()
                            {
                                a.reply = item["text"].as_str().map(str::to_owned);
                            }
                            if let Some(item) = conversation_item(item, now()) {
                                self.stream(id, StreamDelta::ItemCompleted { item });
                            }
                        }
                        (Some("thread/status/changed"), None) if active.is_some() => {
                            let status = &params["status"];
                            let state = match status["type"].as_str() {
                                Some("active") if status["activeFlags"].as_array().is_some_and(|f| f.iter().any(|f| f == "waitingOnApproval")) => Some(AgentState::WaitingForApproval),
                                Some("active") => Some(AgentState::Working),
                                Some("systemError") => Some(AgentState::Error),
                                _ => None,
                            };
                            if let Some(state) = state
                                && state != self.current_state(id)
                            {
                                self.state(id, state, None, None);
                            }
                        }
                        (Some("turn/completed"), None) => {
                            let turn = &params["turn"];
                            let ours = active.as_ref().is_some_and(|a| {
                                a.turn_id.is_none() || a.turn_id.as_deref() == turn["id"].as_str()
                            });
                            if ours {
                                let outcome = match turn["status"].as_str() {
                                    Some("completed") => TurnOutcome::Completed,
                                    Some("interrupted") => TurnOutcome::Interrupted,
                                    _ => TurnOutcome::Failed,
                                };
                                let reason = turn["error"]["message"].as_str().map(str::to_owned);
                                self.end_turn(id, active.take().unwrap(), outcome, reason, &mut steered);
                                items_started.clear();
                                stalled = false;
                            }
                        }
                        _ => {}
                    }
                },
                _ = tokio::time::sleep_until(last + self.inner.config.stalled_after), if active.is_some() && !stalled => {
                    stalled = true;
                    self.state(id, AgentState::Stalled, Some("no Codex output before stalled threshold".into()), None);
                }
                _ = async { tokio::time::sleep_until(kill_at.unwrap()).await }, if kill_at.is_some() => {
                    break None;
                }
            }
        };
        rx.close();
        // Reap the process before acknowledging stop or allowing another start.
        let _ = child.kill().await;
        let _ = child.wait().await;
        let cancelled = failure
            .clone()
            .unwrap_or_else(|| error(ErrorCode::Conflict, "agent stopped before queued turn"));
        if let Some(a) = active {
            self.stream(
                id,
                StreamDelta::TurnEnded {
                    message_id: a.pending.item.id,
                    outcome: TurnOutcome::Failed,
                },
            );
            let _ = a.pending.done.send(Err(cancelled.clone()));
        }
        for p in steered.drain(..).chain(queue.drain(..)) {
            let _ = p.done.send(Err(cancelled.clone()));
        }
        for (_, w) in waiting.drain() {
            if let Waiting::Steer(done) | Waiting::Interrupt(done) = w {
                let _ = done.send(Err(cancelled.clone()));
            }
        }
        while let Ok(control) = rx.try_recv() {
            match control {
                Control::Turn(p) => {
                    let _ = p.done.send(Err(cancelled.clone()));
                }
                Control::Stop(_, done) => stops.push(done),
                Control::Edit(_, _, done) => {
                    let _ = done.send(Err(cancelled.clone()));
                }
                Control::Steer(_, done)
                | Control::SteerQueued(_, done)
                | Control::Withdraw(_, done)
                | Control::Interrupt(done) => {
                    let _ = done.send(Err(cancelled.clone()));
                }
            }
        }
        self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run = None;
        // The stored owner queue is kept, so the next start delivers it.
        self.publish_queue(id, &queue);
        self.state(
            id,
            if failure.is_some() {
                AgentState::Error
            } else {
                AgentState::Stopped
            },
            failure.as_ref().map(ToString::to_string),
            exit_code,
        );
        self.inner
            .agents
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .finished = true;
        for done in stops {
            let _ = done.send(failure.clone().map_or(Ok(()), Err));
        }
    }

    fn end_turn(
        &self,
        id: AgentId,
        active: Active,
        outcome: TurnOutcome,
        reason: Option<String>,
        steered: &mut Vec<Pending>,
    ) {
        self.stream(
            id,
            StreamDelta::TurnEnded {
                message_id: active.pending.item.id,
                outcome,
            },
        );
        self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run = None;
        self.state(
            id,
            if outcome == TurnOutcome::Failed {
                AgentState::Error
            } else {
                AgentState::Idle
            },
            reason,
            None,
        );
        let result = TurnResult {
            outcome,
            result: active.reply,
        };
        let _ = active.pending.done.send(Ok(result.clone()));
        for p in steered.drain(..) {
            let _ = p.done.send(Ok(result.clone()));
        }
    }
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| error(ErrorCode::InvalidParams, e))
}
fn value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(internal)
}

#[async_trait]
impl CommandHandler for CodexSupervisor {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            "agent_start",
            "agent_stop",
            "agent_message_send",
            "agent_message_steer",
            "agent_message_edit",
            "agent_message_withdraw",
            "agent_turn_interrupt",
            "conversation_page",
        ]
    }
    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(error(
                ErrorCode::Denied,
                "main sessions belong to the owner",
            ));
        }
        match call.method.as_str() {
            "agent_start" => {
                let p: AgentStart = params(call.params)?;
                let agent = self.agent(p.agent_id)?;
                self.start(&agent, None).await?;
                value(self.agent(p.agent_id)?)
            }
            "agent_stop" => {
                let p: AgentStop = params(call.params)?;
                self.stop(p.agent_id, p.mode).await?;
                value(self.agent(p.agent_id)?)
            }
            "agent_message_send" => {
                let p: AgentMessageSend = params(call.params)?;
                if p.text.trim().is_empty() {
                    return Err(error(ErrorCode::InvalidParams, "message must not be empty"));
                }
                self.codex_agent(p.agent_id)?;
                let item = QueueItem {
                    id: MessageId::generate(),
                    agent_id: p.agent_id,
                    source: QueueSource::Owner,
                    text: p.text,
                    queued_at: now(),
                };
                // Enqueue before returning; execution continues independently of the socket call.
                let _completion = self.enqueue(p.agent_id, RunId::generate(), item.clone())?;
                value(item)
            }
            "agent_message_steer" => {
                let p: AgentMessageSteer = params(call.params)?;
                self.codex_agent(p.agent_id)?;
                let (done, rx) = oneshot::channel();
                self.sender(p.agent_id)?
                    .send(Control::SteerQueued(p.message_id, done))
                    .map_err(internal)?;
                rx.await.map_err(internal)??;
                value(())
            }
            "agent_message_edit" => {
                let p: AgentMessageEdit = params(call.params)?;
                if p.text.trim().is_empty() {
                    return Err(error(ErrorCode::InvalidParams, "message must not be empty"));
                }
                let (done, rx) = oneshot::channel();
                self.sender(p.agent_id)?
                    .send(Control::Edit(p.message_id, p.text, done))
                    .map_err(internal)?;
                value(rx.await.map_err(internal)??)
            }
            "agent_message_withdraw" => {
                let p: AgentMessageWithdraw = params(call.params)?;
                let (done, rx) = oneshot::channel();
                self.sender(p.agent_id)?
                    .send(Control::Withdraw(p.message_id, done))
                    .map_err(internal)?;
                rx.await.map_err(internal)??;
                value(())
            }
            "agent_turn_interrupt" => {
                let p: AgentTurnInterrupt = params(call.params)?;
                self.interrupt(p.agent_id).await?;
                value(())
            }
            "conversation_page" => {
                let p: ConversationPage = params(call.params)?;
                value(
                    self.conversation(p.agent_id, p.session_id, p.before, p.limit)
                        .await?,
                )
            }
            _ => Err(error(
                ErrorCode::HarnessUnsupported,
                "command not served by Codex supervisor",
            )),
        }
    }
}
