//! Long-lived main-session processes. Each actor owns stdin and its FIFO queue.
use std::{
    collections::{HashMap, VecDeque},
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
    sync::{broadcast, mpsc, oneshot},
    time::Instant,
};

use super::protocol::{self, Launch, LaunchOptions, Priority};
use crate::{
    ask::transcript,
    ports::{
        CommandCall, CommandHandler, EventSink, ForkResult, ForkRun, Harness, Result, RunInfo,
        Store, TurnResult,
    },
};

/// Gate transport configuration is supplied by formationd; this adapter only installs flags.
#[derive(Clone, Debug)]
pub struct ClaudeConfig {
    pub binary: PathBuf,
    pub config_dir: PathBuf,
    pub stalled_after: Duration,
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
}
struct Running {
    tx: mpsc::UnboundedSender<Control>,
    state: AgentState,
    session: SessionInfo,
    seq: u64,
    run: Option<RunInfo>,
    finished: bool,
}
struct Pending {
    run_id: RunId,
    item: QueueItem,
    done: oneshot::Sender<Result<TurnResult>>,
}
enum Control {
    Turn(Pending),
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
        let (streams, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                config,
                store,
                sink,
                agents: Mutex::new(HashMap::new()),
                streams,
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
    fn queue(&self, id: AgentId, queue: &VecDeque<Pending>) {
        self.inner.sink.publish(Event::AgentQueue(AgentQueueEvent {
            agent_id: id,
            items: queue.iter().map(|p| p.item.clone()).collect(),
        }));
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
        let output = Command::new(&self.inner.config.binary)
            .arg("--version")
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(Duration::from_secs(5), output).await;
        let version = match output {
            Ok(Ok(o)) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_owned())
            }
            _ => None,
        };
        HarnessStatus {
            harness: self.kind(),
            installed: version.is_some(),
            signed_in: None,
            version,
            path: Some(self.inner.config.binary.clone()),
            problem: None,
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
            return Ok(r.session.clone());
        }
        let stored = self.inner.store.main_session(agent.id)?;
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
        let args = protocol::launch_args(
            if resume.is_some() {
                Launch::Resume { session_id: &sid.0 }
            } else {
                Launch::New { session_id: &sid.0 }
            },
            LaunchOptions {
                permission_prompt_tool: Some("mcp__formation__permission"),
                mcp_config: Some(&config.mcp_config),
                settings: Some(&settings),
                permission_mode: None,
            },
        );
        let mut child = Command::new(&config.binary)
            .args(args)
            .current_dir(&agent.folder_path)
            .envs(&config.env)
            .env("CLAUDE_CONFIG_DIR", &config.config_dir)
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
        let seq = agents.get(&agent.id).map_or(0, |r| r.seq);
        agents.insert(
            agent.id,
            Running {
                tx,
                state: AgentState::Idle,
                session: session.clone(),
                seq,
                run: None,
                finished: false,
            },
        );
        drop(agents);
        self.state(agent.id, AgentState::Idle, None, None);
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
    async fn run_fork(&self, _: AgentId, _: ForkRun) -> Result<ForkResult> {
        Err(error(
            ErrorCode::HarnessUnsupported,
            "fork execution belongs to TASK-024",
        ))
    }
    async fn steer(&self, _: AgentId, _: QueueItem) -> Result<()> {
        Err(error(
            ErrorCode::HarnessUnsupported,
            "steering belongs to TASK-039",
        ))
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
            .session_info(id)
            .await?
            .ok_or_else(|| error(ErrorCode::NotFound, "no main session"))?;
        if session_id.is_some_and(|sid| sid != session.session_id) {
            return Err(error(
                ErrorCode::HarnessUnsupported,
                "only the main transcript is supported",
            ));
        }
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
        // Disk transcripts are not atomically synchronized with stream output. Zero
        // deliberately makes clients replay buffered deltas rather than lose them.
        Ok(ConversationPageResult {
            items,
            has_more: start > 0,
            stream_seq: 0,
        })
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentStreamEvent> {
        self.inner.streams.subscribe()
    }
}

impl ClaudeSupervisor {
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
        let mut stopping = false;
        let mut stops = Vec::new();
        let mut interrupted = false;
        let mut stalled = false;
        let mut last = Instant::now();
        let mut message = String::new();
        let mut steps = HashMap::new();
        let mut exit_code = None;
        let failure = loop {
            if active.is_none() {
                if stopping {
                    break None;
                }
                if let Some(p) = queue.pop_front() {
                    self.queue(id, &queue);
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
                        else {queue.push_back(p);self.queue(id,&queue);}
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
                    }
                    None => break None,
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
                            if value["isReplay"]!=true {
                                for item in super::conversation::line_items(&value,now()) {
                                    if let ConversationItem::Step{id,..}=&item {steps.insert(id.0.clone(),item.clone());}
                                    self.stream(id,StreamDelta::ItemCompleted{item});
                                }
                                for item in super::conversation::tool_results(&value,&mut steps) {self.stream(id,StreamDelta::ItemCompleted{item});}
                            }
                        }
                        protocol::Event::Result(result) => {
                            if let Some(p)=active.take() {
                                stalled=false;
                                let outcome=if interrupted && result.subtype=="error_during_execution" {TurnOutcome::Interrupted} else if result.is_error || result.subtype!="success" {TurnOutcome::Failed} else {TurnOutcome::Completed};
                                self.stream(id,StreamDelta::TurnEnded{message_id:p.item.id,outcome});
                                self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run=None;
                                self.state(id,if outcome==TurnOutcome::Failed {AgentState::Error} else {AgentState::Idle},result.terminal_reason,None);
                                let _=p.done.send(Ok(TurnResult{outcome,result:result.result}));
                            }
                        }
                        _=>{}
                    }
                },
                _ = tokio::time::sleep_until(last+self.inner.config.stalled_after), if active.is_some() && !stalled => {
                    stalled=true;self.state(id,AgentState::Stalled,Some("no Claude output before stalled threshold".into()),None);
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
        for p in queue.drain(..) {
            let _ = p.done.send(Err(cancelled.clone()));
        }
        while let Ok(control) = rx.try_recv() {
            match control {
                Control::Turn(p) => {
                    let _ = p.done.send(Err(cancelled.clone()));
                }
                Control::Stop(_, done) => stops.push(done),
                Control::Interrupt(done) => {
                    let _ = done.send(Err(cancelled.clone()));
                }
            }
        }
        self.inner.agents.lock().unwrap().get_mut(&id).unwrap().run = None;
        self.queue(id, &queue);
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
                "command not served by Claude supervisor",
            )),
        }
    }
}
