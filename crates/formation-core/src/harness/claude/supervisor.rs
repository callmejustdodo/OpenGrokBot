//! Long-lived main-session processes. Each actor owns stdin and its FIFO queue.
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
    policy::Caller,
    request::{QueueItem, QueueSource},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Notify, broadcast, mpsc, oneshot},
    time::Instant,
};

use super::protocol::{self, Launch, LaunchOptions, Priority};
use crate::{
    agents::{ClaudeReadiness, HarnessReadiness},
    ask::transcript,
    history,
    ports::{
        CommandCall, CommandHandler, EventSink, ForkResult, ForkRun, ForkTools, Harness,
        RequestQueue, Result, RunInfo, RunProgress, Store, TurnResult,
    },
};

/// Names the run of a Claude Code process for the permission tool and hook.
pub const RUN_ID_ENV: &str = "FORMATION_RUN_ID";
/// Names the agent of a Claude Code process for the permission tool and hook.
pub const AGENT_ID_ENV: &str = "FORMATION_AGENT_ID";

/// Added to every gated launch (with `--permission-mode default`), so no user,
/// project or local allow rule or mode lets a tool call skip the permission tool
/// (#136, `PermissionVerdict::Defer`). `--settings` still loads the hook.
pub(super) const GATED_ARGS: [&str; 2] = ["--setting-sources", ""];

/// Gate transport configuration is supplied by formationd; this adapter only installs flags.
#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    pub binary: PathBuf,
    pub config_dir: PathBuf,
    pub stalled_after: Duration,
    /// After "stop now", how long a turn or request fork may run before it is killed.
    pub stop_timeout: Duration,
    pub mcp_config: String,
    pub pre_tool_use_command: String,
    /// Child-only environment, including fake-claude fixture selection in tests.
    pub env: HashMap<String, String>,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            binary: "claude".into(),
            config_dir: transcript::claude_config_dir().unwrap_or_else(|| PathBuf::from(".claude")),
            stalled_after: Duration::from_secs(600),
            stop_timeout: Duration::from_secs(10),
            mcp_config: json!({"mcpServers":{"formation":{"command":"formation","args":["mcp","--permission"]}}}).to_string(),
            pre_tool_use_command: "formation hook pre-tool-use".into(),
            env: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct ClaudeSupervisor {
    inner: Arc<Inner>,
}
struct Inner {
    config: ClaudeConfig,
    store: Arc<dyn Store>,
    sink: Arc<dyn EventSink>,
    agents: Mutex<HashMap<AgentId, Running>>,
    streams: broadcast::Sender<AgentStreamEvent>,
    requests: Option<Arc<dyn RequestQueue>>,
    forks: Mutex<HashMap<RunId, RunInfo>>,
    /// Teammates' requests running in forks, with the signal that stops each one.
    request_stops: Mutex<HashMap<RequestId, (AgentId, Arc<Notify>)>>,
}
struct Running {
    tx: mpsc::UnboundedSender<Control>,
    state: AgentState,
    session: SessionInfo,
    seq: u64,
    /// The last `seq` whose items Claude has written to the transcript: every turn
    /// up to the last one that ended with a result.
    transcript_seq: u64,
    run: Option<RunInfo>,
    /// `FORMATION_RUN_ID` of the long-lived process: the permission tool and hook
    /// name it, and [`Harness::run`] answers with the turn running at that moment.
    process_run: RunId,
    finished: bool,
}
struct Pending {
    run_id: RunId,
    item: QueueItem,
    done: oneshot::Sender<Result<TurnResult>>,
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
fn error(code: ErrorCode, message: impl ToString) -> ApiError {
    ApiError::new(code, message.to_string())
}
fn internal(message: impl ToString) -> ApiError {
    error(ErrorCode::Internal, message)
}
fn now() -> Timestamp {
    SystemTime::now().into()
}

impl ClaudeSupervisor {
    pub fn new(config: ClaudeConfig, store: Arc<dyn Store>, sink: Arc<dyn EventSink>) -> Self {
        Self::with_request_queue(config, store, sink, None)
    }

    pub fn with_request_queue(
        config: ClaudeConfig,
        store: Arc<dyn Store>,
        sink: Arc<dyn EventSink>,
        requests: Option<Arc<dyn RequestQueue>>,
    ) -> Self {
        let (streams, _) = broadcast::channel(256);
        // No fork or process outlives the daemon that ran it: drop lists a previous run
        // left, and stop agents it left running. An error stays until the next start.
        if let Ok(agents) = store.agents() {
            for mut agent in agents {
                let running = agent.harness == HarnessKind::ClaudeCode
                    && agent.detected.is_none()
                    && !matches!(agent.state, AgentState::Stopped | AgentState::Error);
                if !agent.running_requests.is_empty() || running {
                    agent.running_requests.clear();
                    if running {
                        agent.state = AgentState::Stopped;
                    }
                    let _ = store.update_agent(&agent);
                }
            }
        }
        Self {
            inner: Arc::new(Inner {
                config,
                store,
                sink,
                agents: Mutex::new(HashMap::new()),
                streams,
                requests,
                forks: Mutex::new(HashMap::new()),
                request_stops: Mutex::new(HashMap::new()),
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
        // The snapshot reads agents from the store, so it must hold the last state.
        // Writing under the lock keeps the stored order the same as the events'.
        if let Ok(Some(mut agent)) = self.inner.store.agent(id)
            && agent.state != state
        {
            agent.state = state;
            let _ = self.inner.store.update_agent(&agent);
        }
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
    /// Changes the agent's running requests in the store and publishes `agent.update`;
    /// `change` returns whether it changed anything.
    fn update_running(
        &self,
        id: AgentId,
        change: impl FnOnce(&mut Vec<RunningRequest>) -> bool,
    ) -> Result<()> {
        let mut agent = self
            .inner
            .store
            .agent(id)?
            .ok_or_else(|| error(ErrorCode::NotFound, "agent not found"))?;
        if !change(&mut agent.running_requests) {
            return Ok(());
        }
        if let Some(r) = self.inner.agents.lock().unwrap().get(&id) {
            agent.state = r.state;
        }
        self.inner.store.update_agent(&agent)?;
        self.inner
            .sink
            .publish(Event::AgentUpdate(AgentUpdateEvent::Upsert {
                agent: Box::new(agent),
            }));
        Ok(())
    }
    /// Records or clears an authentication failure and, when that changes the
    /// status, publishes `harness.update` without blocking the caller.
    fn auth_problem(&self, problem: Option<String>) {
        if !ClaudeReadiness::set_auth_problem(&self.inner.config.binary, problem) {
            return;
        }
        let supervisor = self.clone();
        tokio::spawn(async move {
            let status = supervisor.status().await;
            supervisor
                .inner
                .sink
                .publish(Event::HarnessUpdate(HarnessUpdateEvent {
                    harnesses: vec![status],
                }));
        });
    }
    /// Runs a teammate's request in a fork, listed on the agent while it runs.
    async fn run_request(
        &self,
        id: AgentId,
        request_id: RequestId,
        requester: MemberId,
        fork: ForkRun,
    ) -> Result<TurnResult> {
        let stop = Arc::new(Notify::new());
        self.inner
            .request_stops
            .lock()
            .unwrap()
            .insert(request_id, (id, stop.clone()));
        let run_id = fork.run_id;
        let entry = RunningRequest {
            request_id,
            requester,
            prompt: fork.item.text.clone(),
            started_at: now(),
            run_id,
            fork_session_id: None,
        };
        let result = match self.update_running(id, |list| {
            list.push(entry);
            true
        }) {
            Err(e) => Err(e),
            Ok(()) => tokio::select! {
                result = self.run_fork(id, fork) => result.map(|fork| fork.result),
                _ = stop.notified() => {
                    // Dropping the fork future kills its process (`kill_on_drop`).
                    self.inner.forks.lock().unwrap().remove(&run_id);
                    Ok(TurnResult { outcome: TurnOutcome::Interrupted, result: None })
                }
            },
        };
        self.inner.request_stops.lock().unwrap().remove(&request_id);
        let _ = self.update_running(id, |list| {
            let before = list.len();
            list.retain(|r| r.request_id != request_id);
            list.len() != before
        });
        result
    }
}

async fn write(stdin: &mut ChildStdin, line: String) -> Result<()> {
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .await
        .map_err(internal)?;
    stdin.flush().await.map_err(internal)
}

#[async_trait]
impl Harness for ClaudeSupervisor {
    fn kind(&self) -> HarnessKind {
        HarnessKind::ClaudeCode
    }
    async fn status(&self) -> HarnessStatus {
        // `claude auth status --json` reports sign-in without starting a model turn.
        let readiness = ClaudeReadiness::new(self.inner.config.binary.clone());
        let check = tokio::task::spawn_blocking(move || readiness.check());
        match tokio::time::timeout(Duration::from_secs(10), check).await {
            Ok(Ok(Ok(mut statuses))) if !statuses.is_empty() => statuses.remove(0),
            _ => HarnessStatus {
                harness: self.kind(),
                installed: false,
                signed_in: None,
                version: None,
                path: Some(self.inner.config.binary.clone()),
                problem: Some("Could not check Claude Code".into()),
            },
        }
    }
    async fn start(&self, agent: &Agent, resume: Option<SessionId>) -> Result<SessionInfo> {
        if agent.harness != HarnessKind::ClaudeCode || agent.detected.is_some() {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only Formation Claude Code agents can start",
            ));
        }
        // No await between checking and registering: concurrent starts cannot spawn twice.
        let mut agents = self.inner.agents.lock().unwrap();
        if let Some(r) = agents.get(&agent.id).filter(|r| !r.finished) {
            let session = r.session.clone();
            // The process outlived a failed turn: starting again clears the error.
            let failed = r.state == AgentState::Error;
            drop(agents);
            if failed {
                self.state(agent.id, AgentState::Idle, None, None);
            }
            return Ok(session);
        }
        let stored = self.inner.store.main_session(agent.id)?;
        let restored = self.inner.store.owner_queue(agent.id)?;
        let resume = resume.or_else(|| stored.as_ref().map(|s| s.session_id.clone()));
        let sid = resume
            .clone()
            .unwrap_or_else(|| SessionId(RunId::generate().to_string()));
        let config = &self.inner.config;
        let path = transcript::find_transcript(&config.config_dir, &agent.folder_path, &sid.0)
            .map_err(internal)?
            .unwrap_or(
                transcript::transcript_path(&config.config_dir, &agent.folder_path, &sid.0)
                    .map_err(internal)?,
            );
        let session = SessionInfo {
            agent_id: agent.id,
            session_id: sid.clone(),
            transcript_path: path,
            cwd: agent.folder_path.clone(),
            started_at: now(),
            last_activity_at: stored.and_then(|s| s.last_activity_at),
        };
        let settings = json!({"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":config.pre_tool_use_command}]}]}}).to_string();
        let mut args = protocol::launch_args(
            if resume.is_some() {
                Launch::Resume { session_id: &sid.0 }
            } else {
                Launch::New { session_id: &sid.0 }
            },
            LaunchOptions {
                permission_prompt_tool: Some("mcp__formation__permission"),
                mcp_config: Some(&config.mcp_config),
                settings: Some(&settings),
                permission_mode: Some("default"),
            },
        );
        args.extend(GATED_ARGS.map(String::from));
        let process_run = RunId::generate();
        let mut child = Command::new(&config.binary)
            .args(args)
            .current_dir(&agent.folder_path)
            .envs(&config.env)
            .env("CLAUDE_CONFIG_DIR", &config.config_dir)
            .env(RUN_ID_ENV, process_run.to_string())
            .env(AGENT_ID_ENV, agent.id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| error(ErrorCode::HarnessUnavailable, e))?;
        self.inner.store.put_main_session(&session)?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
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
        let (seq, transcript_seq) = agents
            .get(&agent.id)
            .map_or((0, 0), |r| (r.seq, r.transcript_seq));
        agents.insert(
            agent.id,
            Running {
                tx,
                state: AgentState::Idle,
                session: session.clone(),
                seq,
                transcript_seq,
                run: None,
                process_run,
                finished: false,
            },
        );
        drop(agents);
        self.state(agent.id, AgentState::Idle, None, None);
        let _ = history::record(
            self.inner.store.as_ref(),
            ActivityKind::AgentStarted,
            Some(agent.id),
            None,
            now(),
            agent.name.clone(),
        );
        let supervisor = self.clone();
        let id = agent.id;
        tokio::spawn(async move {
            supervisor
                .supervise(id, child, stdin, BufReader::new(stdout).lines(), rx)
                .await;
        });
        Ok(session)
    }
    async fn run_turn(&self, id: AgentId, run_id: RunId, item: QueueItem) -> Result<TurnResult> {
        self.enqueue(id, run_id, item)?.await.map_err(internal)?
    }
    async fn run_fork(&self, id: AgentId, fork: ForkRun) -> Result<ForkResult> {
        let agent = self.agent(id)?;
        if agent.harness != HarnessKind::ClaudeCode {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "not a Claude Code agent",
            ));
        }
        if fork.item.agent_id != id {
            return Err(error(
                ErrorCode::InvalidParams,
                "queue item belongs to another agent",
            ));
        }
        self.inner.forks.lock().unwrap().insert(
            fork.run_id,
            RunInfo {
                run_id: fork.run_id,
                agent_id: id,
                session_id: fork.from.clone(),
                item: fork.item.clone(),
            },
        );
        let result = self.execute_fork(&agent, &fork).await;
        self.inner.forks.lock().unwrap().remove(&fork.run_id);
        result
    }
    async fn steer(&self, id: AgentId, item: QueueItem) -> Result<()> {
        let agent = self.agent(id)?;
        if agent.harness != HarnessKind::ClaudeCode || agent.detected.is_some() {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "detected agents are read and fork only",
            ));
        }
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
        let Some(mut session) = self.inner.store.main_session(id)? else {
            return Ok(None);
        };
        if let Some(path) = transcript::find_transcript(
            &self.inner.config.config_dir,
            &session.cwd,
            &session.session_id.0,
        )
        .map_err(internal)?
        {
            session.transcript_path = path;
        }
        Ok(Some(session))
    }
    async fn run(&self, id: RunId) -> Option<RunInfo> {
        if let Some(run) = self.inner.forks.lock().unwrap().get(&id).cloned() {
            return Some(run);
        }
        self.inner.agents.lock().unwrap().values().find_map(|r| {
            r.run
                .as_ref()
                .filter(|run| run.run_id == id || r.process_run == id)
                .cloned()
        })
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
            .session_info(id)
            .await?
            .ok_or_else(|| error(ErrorCode::NotFound, "no main session"))?;
        if session_id.is_some_and(|sid| sid != session.session_id) {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only the main transcript is supported",
            ));
        }
        // Read the watermark before the transcript, so it never claims items the
        // page has not read.
        let stream_seq = self
            .inner
            .agents
            .lock()
            .unwrap()
            .get(&id)
            .map_or(0, |r| r.transcript_seq);
        let path = transcript::find_transcript(
            &self.inner.config.config_dir,
            &session.cwd,
            &session.session_id.0,
        )
        .map_err(internal)?
        .unwrap_or(session.transcript_path);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(internal(e)),
        };
        let mut items = super::conversation::read_items(&text)?;
        let end = match before {
            Some(before) => items
                .iter()
                .position(|item| super::conversation::item_id(item) == &before)
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

/// The `agent.state` reason, and so the inbox subject, of a failed turn: Claude's
/// own text on one line, then the terminal reason in parentheses (#185). A usage
/// limit says so; Claude's text carries its reset time.
fn failure_reason(result: &protocol::ResultEvent) -> String {
    let code = result.terminal_reason.as_deref().unwrap_or(&result.subtype);
    let text = result
        .result
        .as_deref()
        .and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|l| match l.char_indices().nth(200) {
            Some((i, _)) => format!("{}…", &l[..i]),
            None => l.to_owned(),
        });
    let lower = text.as_deref().unwrap_or_default().to_lowercase();
    let usage_limit = result.api_error_status.as_ref().and_then(Value::as_u64) == Some(429)
        || ["usage limit", "limit reached", "hit your limit", "resets"]
            .iter()
            .any(|k| lower.contains(k));
    match (usage_limit, text) {
        (true, Some(text)) => format!("Claude usage limit: {text} ({code})"),
        (true, None) => format!("Claude usage limit ({code})"),
        (false, Some(text)) => format!("{text} ({code})"),
        (false, None) => code.to_owned(),
    }
}

fn queue_error(message_id: MessageId, started: &HashSet<MessageId>) -> ApiError {
    if started.contains(&message_id) {
        error(ErrorCode::Conflict, "message already started")
    } else {
        error(ErrorCode::NotFound, "queued message not found")
    }
}

impl ClaudeSupervisor {
    async fn send_steer(
        &self,
        id: AgentId,
        stdin: &mut ChildStdin,
        item: &QueueItem,
    ) -> Result<()> {
        write(stdin, protocol::encode_user_message(&item.text, None)).await?;
        self.stream(
            id,
            StreamDelta::ItemCompleted {
                item: ConversationItem::UserMessage {
                    id: ItemId(item.id.to_string()),
                    at: now(),
                    author: Author::Owner,
                    text: item.text.clone(),
                },
            },
        );
        Ok(())
    }

    async fn supervise(
        &self,
        id: AgentId,
        mut child: Child,
        mut stdin: ChildStdin,
        mut lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        mut rx: mpsc::UnboundedReceiver<Control>,
    ) {
        let mut queue = VecDeque::<Pending>::new();
        let mut active: Option<Pending> = None;
        let mut steered = Vec::<Pending>::new();
        // Steers written in the running turn that Claude has not yet replayed. Claude
        // answers a steer that arrives after the turn's last tool call with a result of
        // its own, so the turn ends with the result that follows the last replay.
        let mut unreplayed = Vec::<String>::new();
        let mut started = HashSet::new();
        let mut stopping = false;
        let mut stops = Vec::new();
        let mut interrupted = false;
        let mut stalled = false;
        let mut last = Instant::now();
        let mut message = String::new();
        let mut steps = HashMap::new();
        let mut exit_code = None;
        let requests = self.inner.requests.clone();
        let mut request_run = None;
        let mut request_message = None;
        let mut request_blocks_main = false;
        let mut kill_at: Option<Instant> = None;
        let failure = loop {
            if active.is_none() && (!request_blocks_main || request_run.is_none()) {
                if stopping && request_run.is_none() {
                    break None;
                }
                if let Some(p) = queue.pop_front() {
                    started.insert(p.item.id);
                    if let Err(e) = self.queue(id, &queue) {
                        break Some(e);
                    }
                    let info = RunInfo {
                        run_id: p.run_id,
                        agent_id: id,
                        session_id: self.inner.agents.lock().unwrap()[&id]
                            .session
                            .session_id
                            .clone(),
                        item: p.item.clone(),
                    };
                    self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run = Some(info);
                    self.state(id, AgentState::Working, None, None);
                    self.stream(
                        id,
                        StreamDelta::TurnStarted {
                            message_id: p.item.id,
                        },
                    );
                    self.stream(
                        id,
                        StreamDelta::ItemCompleted {
                            item: ConversationItem::UserMessage {
                                id: ItemId(p.item.id.to_string()),
                                at: now(),
                                author: match p.item.source {
                                    QueueSource::Schedule { schedule_id } => {
                                        Author::Schedule { schedule_id }
                                    }
                                    _ => Author::Owner,
                                },
                                text: p.item.text.clone(),
                            },
                        },
                    );
                    let sent = write(
                        &mut stdin,
                        protocol::encode_user_message(&p.item.text, Some(Priority::Later)),
                    )
                    .await;
                    active = Some(p);
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
                        if stopping {let _=p.done.send(Err(error(ErrorCode::Conflict,"agent is stopping")));}
                        else {queue.push_back(p);if let Err(e)=self.queue(id,&queue) {break Some(e);}}
                    }
                    Some(Control::Steer(item, done)) => {
                        let result = if stopping {
                            Err(error(ErrorCode::Conflict, "agent is stopping"))
                        } else if active.is_none() {
                            Err(error(ErrorCode::Conflict, "agent has no running turn"))
                        } else {
                            let result = self.send_steer(id, &mut stdin, &item).await;
                            if result.is_ok() {started.insert(item.id);unreplayed.push(item.text);}
                            result
                        };
                        let fatal = result.as_ref().err().filter(|e| e.code == ErrorCode::Internal).cloned();
                        let _ = done.send(result);
                        if fatal.is_some() {break fatal;}
                    }
                    Some(Control::SteerQueued(message_id, done)) => {
                        let result = if stopping {
                            Err(error(ErrorCode::Conflict, "agent is stopping"))
                        } else if active.is_none() {
                            Err(error(ErrorCode::Conflict, "agent has no running turn"))
                        } else if let Some(index) = queue.iter().position(|p| p.item.id == message_id) {
                            if queue[index].item.source != QueueSource::Owner {
                                Err(error(ErrorCode::Denied, "only owner messages can steer"))
                            } else {
                                match self.send_steer(id, &mut stdin, &queue[index].item).await {
                                    Ok(()) => {
                                        started.insert(message_id);
                                        unreplayed.push(queue[index].item.text.clone());
                                        steered.push(queue.remove(index).unwrap());
                                        self.queue(id, &queue)
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                        } else {
                            Err(queue_error(message_id, &started))
                        };
                        let fatal = result.as_ref().err().filter(|e| e.code == ErrorCode::Internal).cloned();
                        let _ = done.send(result);
                        if fatal.is_some() {break fatal;}
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
                        let result = if active.is_some() {interrupted=true;write(&mut stdin,protocol::encode_interrupt(&RunId::generate().to_string())).await} else {Err(error(ErrorCode::Conflict,"no running turn"))};
                        let fatal = result.as_ref().err().filter(|e|e.code==ErrorCode::Internal).cloned();
                        let _=done.send(result);
                        if fatal.is_some() {break fatal;}
                    }
                    Some(Control::Stop(mode,done)) => {
                        stopping=true;
                        stops.push(done);
                        if mode==StopMode::Now && active.is_some() && !interrupted {
                            interrupted=true;
                            if let Err(e)=write(&mut stdin,protocol::encode_interrupt(&RunId::generate().to_string())).await {break Some(e);}
                        }
                        // A turn or fork that ignores the interrupt is killed after the timeout.
                        if mode==StopMode::Now && kill_at.is_none() && (active.is_some() || request_run.is_some()) {
                            kill_at=Some(Instant::now()+self.inner.config.stop_timeout);
                        }
                    }
                    None => break None,
                },
                item = async { requests.as_ref().unwrap().next(id).await },
                    if requests.is_some() && active.is_none() && request_run.is_none() && !stopping => {
                    let item = match item { Ok(item) => item, Err(e) => break Some(e) };
                    let port = requests.as_ref().unwrap();
                    if let Err(e) = port.report(item.id, RunProgress::Started).await { break Some(e); }
                    request_blocks_main = matches!(item.source, QueueSource::Request { .. });
                    request_message = Some(item.id);
                    let supervisor = self.clone();
                    let from = self.inner.agents.lock().unwrap()[&id].session.session_id.clone();
                    request_run = Some(Box::pin(async move {
                        let message_id = item.id;
                        let result = if let QueueSource::Request { request_id, requester } = item.source {
                            supervisor.run_request(id, request_id, requester, ForkRun {
                                run_id: RunId::generate(), from, item, tools: ForkTools::Gated,
                            }).await
                        } else {
                            supervisor.run_turn(id, RunId::generate(), item).await
                        };
                        (message_id, result)
                    }) as std::pin::Pin<Box<dyn std::future::Future<Output = (MessageId, Result<TurnResult>)> + Send>>);
                },
                (message_id, result) = async { request_run.as_mut().unwrap().await }, if request_run.is_some() => {
                    request_run = None;
                    request_message = None;
                    let progress = match result {
                        Ok(TurnResult { outcome: TurnOutcome::Completed, result }) => RunProgress::Completed { result },
                        Ok(TurnResult { outcome: TurnOutcome::Interrupted, .. }) => RunProgress::Interrupted,
                        Ok(turn) => RunProgress::Failed { error: internal(turn.result.unwrap_or_else(|| "fork failed".into())) },
                        Err(error) => RunProgress::Failed { error },
                    };
                    if let Err(e) = requests.as_ref().unwrap().report(message_id, progress).await { break Some(e); }
                },
                line = lines.next_line() => {
                    let line = match line {Ok(Some(line))=>line,Ok(None)=> {
                        let status=child.wait().await;
                        exit_code=status.as_ref().ok().and_then(|s|s.code());
                        break Some(error(ErrorCode::HarnessUnavailable,format!("Claude exited: {status:?}")));
                    },Err(e)=>break Some(internal(e))};
                    last=Instant::now();
                    if stalled {stalled=false;self.state(id,AgentState::Working,None,None);}
                    let event=match protocol::decode_stdout_line(&line) {Ok(e)=>e,Err(e)=>break Some(internal(e))};
                    let activity = {
                        let mut agents=self.inner.agents.lock().unwrap();
                        let r=agents.get_mut(&id).unwrap();
                        r.session.last_activity_at=Some(now());
                        r.session.clone()
                    };
                    if let Err(e)=self.inner.store.put_main_session(&activity) {break Some(e);}
                    match event {
                        protocol::Event::System(protocol::SystemEvent{session_id,kind:protocol::SystemKind::Init{..}}) if session_id!=activity.session_id.0 => {
                            break Some(internal("Claude initialized a different session"));
                        }
                        protocol::Event::StreamEvent(e) => match e.event {
                            protocol::ApiEvent::MessageStart{message:m}=>message=m.id,
                            protocol::ApiEvent::ContentBlockStart{index,content_block:protocol::ContentBlock::Text{text}}=>self.stream(id,StreamDelta::ItemStarted{item:ConversationItem::AssistantText{id:ItemId(format!("{message}:{index}")),at:now(),text}}),
                            protocol::ApiEvent::ContentBlockDelta{index,delta:protocol::ContentDelta::Text{text}}=>self.stream(id,StreamDelta::TextDelta{item_id:ItemId(format!("{message}:{index}")),text}),
                            _=>{}
                        },
                        protocol::Event::Assistant(_) | protocol::Event::User(_) => {
                            let value:Value=serde_json::from_str(&line).expect("decoded JSON");
                            // Input is already emitted at dispatch. Tool results still update steps.
                            if value["isReplay"]==true {
                                if let Some(i)=unreplayed.iter().position(|t| value["message"]["content"].as_str()==Some(t)) {unreplayed.remove(i);}
                            } else {
                                for item in super::conversation::line_items(&value,now()) {
                                    if let ConversationItem::Step{id,..}=&item {steps.insert(id.0.clone(),item.clone());}
                                    self.stream(id,StreamDelta::ItemCompleted{item});
                                }
                                for item in super::conversation::tool_results(&value,&mut steps) {self.stream(id,StreamDelta::ItemCompleted{item});}
                            }
                        }
                        protocol::Event::Result(result) => {
                            // A steer not yet replayed gets its own result; an interrupt may drop it.
                            if !unreplayed.is_empty() && !interrupted {continue;}
                            unreplayed.clear();
                            if let Some(p)=active.take() {
                                stalled=false;
                                let outcome=if interrupted && result.subtype=="error_during_execution" {TurnOutcome::Interrupted} else if result.is_error || result.subtype!="success" {TurnOutcome::Failed} else {TurnOutcome::Completed};
                                self.stream(id,StreamDelta::TurnEnded{message_id:p.item.id,outcome});
                                {let mut agents=self.inner.agents.lock().unwrap();let r=agents.get_mut(&id).unwrap();r.transcript_seq=r.seq;}
                                self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run=None;
                                if outcome==TurnOutcome::Failed {
                                    self.state(id,AgentState::Error,Some(failure_reason(&result)),None);
                                } else {
                                    self.state(id,AgentState::Idle,result.terminal_reason,None);
                                }
                                // Claude reports an expired sign-in only as the turn's error text.
                                match outcome {
                                    TurnOutcome::Completed => self.auth_problem(None),
                                    TurnOutcome::Failed => if let Some(text)=result.result.as_deref().filter(|t| t.contains("Failed to authenticate")) {
                                        self.auth_problem(Some(format!("Sign in to Claude Code again with claude auth login ({text})")));
                                    },
                                    TurnOutcome::Interrupted => {}
                                }
                                let turn_result = TurnResult{outcome,result:result.result};
                                let _=p.done.send(Ok(turn_result.clone()));
                                for p in steered.drain(..) {let _ = p.done.send(Ok(turn_result.clone()));}
                            }
                        }
                        _=>{}
                    }
                },
                _ = tokio::time::sleep_until(last+self.inner.config.stalled_after), if active.is_some() && !stalled => {
                    stalled=true;self.state(id,AgentState::Stalled,Some("no Claude output before stalled threshold".into()),None);
                }
                _ = async { tokio::time::sleep_until(kill_at.unwrap()).await }, if kill_at.is_some() => {
                    // Dropping the fork's future kills its process (`kill_on_drop`); the main
                    // process is killed below.
                    if request_run.take().is_some() {
                        self.inner.forks.lock().unwrap().retain(|_, run| run.agent_id != id);
                        if let Some(message_id) = request_message.take()
                            && let Err(e) = requests.as_ref().unwrap().report(message_id, RunProgress::Interrupted).await
                        {
                            break Some(e);
                        }
                    }
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
        if let Some(p) = active {
            self.stream(
                id,
                StreamDelta::TurnEnded {
                    message_id: p.item.id,
                    outcome: TurnOutcome::Failed,
                },
            );
            let _ = p.done.send(Err(cancelled.clone()));
        }
        for p in steered.drain(..) {
            let _ = p.done.send(Err(cancelled.clone()));
        }
        for p in queue.drain(..) {
            let _ = p.done.send(Err(cancelled.clone()));
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
        let (kind, summary) = match &failure {
            Some(failure) => (ActivityKind::AgentFailed, failure.message.clone()),
            None => (
                ActivityKind::AgentStopped,
                self.inner
                    .store
                    .agent(id)
                    .ok()
                    .flatten()
                    .map_or_else(|| id.to_string(), |agent| agent.name),
            ),
        };
        let _ = history::record(
            self.inner.store.as_ref(),
            kind,
            Some(id),
            None,
            now(),
            summary,
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
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| error(ErrorCode::InvalidParams, e))
}
fn value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(internal)
}

#[async_trait]
impl CommandHandler for ClaudeSupervisor {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            "agent_start",
            "agent_stop",
            "agent_message_send",
            "agent_message_steer",
            "agent_message_edit",
            "agent_message_withdraw",
            "agent_turn_interrupt",
            "agent_request_stop",
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
                let agent = self.agent(p.agent_id)?;
                if agent.harness != HarnessKind::ClaudeCode || agent.detected.is_some() {
                    return Err(error(
                        ErrorCode::HarnessUnsupported,
                        "detected agents are read and fork only",
                    ));
                }
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
                let agent = self.agent(p.agent_id)?;
                if agent.harness != HarnessKind::ClaudeCode || agent.detected.is_some() {
                    return Err(error(
                        ErrorCode::HarnessUnsupported,
                        "detected agents are read and fork only",
                    ));
                }
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
            "agent_request_stop" => {
                let p: AgentRequestStop = params(call.params)?;
                let stop = self
                    .inner
                    .request_stops
                    .lock()
                    .unwrap()
                    .get(&p.request_id)
                    .filter(|(agent_id, _)| *agent_id == p.agent_id)
                    .map(|(_, stop)| stop.clone())
                    .ok_or_else(|| {
                        error(ErrorCode::NotFound, "request is not running on this agent")
                    })?;
                stop.notify_one();
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
                "command not served by Claude supervisor",
            )),
        }
    }
}

#[path = "fork.rs"]
mod fork;

#[cfg(test)]
mod tests {
    use super::*;

    fn result(text: Option<&str>, status: Value) -> protocol::ResultEvent {
        protocol::ResultEvent {
            session_id: "s".into(),
            subtype: "success".into(),
            is_error: true,
            result: text.map(str::to_owned),
            terminal_reason: Some("api_error".into()),
            api_error_status: Some(status),
            total_cost_usd: None,
            usage: None,
        }
    }

    #[test]
    fn failure_reason_names_a_usage_limit_with_claude_text() {
        assert_eq!(
            failure_reason(&result(
                Some("5-hour limit reached · resets 3:40pm (Asia/Seoul)"),
                Value::Null
            )),
            "Claude usage limit: 5-hour limit reached · resets 3:40pm (Asia/Seoul) (api_error)"
        );
        assert_eq!(
            failure_reason(&result(None, json!(429))),
            "Claude usage limit (api_error)"
        );
    }

    #[test]
    fn failure_reason_keeps_other_errors_on_one_line() {
        assert_eq!(
            failure_reason(&result(Some("\nAPI Error: 500\nbody"), Value::Null)),
            "API Error: 500 (api_error)"
        );
        assert_eq!(failure_reason(&result(None, Value::Null)), "api_error");
    }
}
