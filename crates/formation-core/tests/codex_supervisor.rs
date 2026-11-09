#![cfg(feature = "testkit")]
use formation_core::{
    harness::codex::{CodexConfig, CodexSupervisor},
    ports::*,
    testkit::{FakePermissionGate, MemoryStore, RecordingEventSink},
};
use formation_proto::{
    agent::*,
    common::*,
    events::Event,
    harness::*,
    policy::Caller,
    request::{QueueItem, QueueSource},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

const RECORDED_THREAD: &str = "01a1bed0-c036-7df2-8669-e209dbd69481";

struct Setup {
    root: PathBuf,
    agent: Agent,
    store: Arc<MemoryStore>,
    sink: Arc<RecordingEventSink>,
    supervisor: CodexSupervisor,
}
impl Setup {
    fn new(turns: &str, delay: u64, gate: Option<Arc<dyn PermissionGate>>) -> Self {
        let root = std::env::temp_dir().join(format!("formation-codex-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "codex".into(),
            name: "Codex".into(),
            role: "developer".into(),
            harness: HarnessKind::Codex,
            folder_path: root.clone(),
            repositories: vec![],
            avatar: Avatar {
                shape: "circle".into(),
                color: "green".into(),
            },
            state: AgentState::Stopped,
            files_revision: "1".into(),
            created_at: SystemTime::now().into(),
            last_activity_at: None,
            detected: None,
            running_requests: vec![],
        };
        let store = Arc::new(MemoryStore::default());
        store.insert_agent(&agent).unwrap();
        let sink = Arc::new(RecordingEventSink::default());
        let config = CodexConfig {
            binary: env!("CARGO_BIN_EXE_fake-codex").into(),
            env: HashMap::from([
                ("FAKE_CODEX_TURNS".into(), turns.into()),
                ("FAKE_CODEX_LINE_DELAY_MS".into(), delay.to_string()),
                (
                    "FAKE_CODEX_LOG".into(),
                    root.join("log.jsonl").display().to_string(),
                ),
            ]),
            ..CodexConfig::default()
        };
        let supervisor = CodexSupervisor::new(config, store.clone(), sink.clone(), gate);
        Self {
            root,
            agent,
            store,
            sink,
            supervisor,
        }
    }
    fn item(&self, text: &str) -> QueueItem {
        QueueItem {
            id: MessageId::generate(),
            agent_id: self.agent.id,
            source: QueueSource::Owner,
            text: text.into(),
            queued_at: SystemTime::now().into(),
        }
    }
    fn spawn_turn(&self, text: &str) -> tokio::task::JoinHandle<Result<TurnResult>> {
        let supervisor = self.supervisor.clone();
        let item = self.item(text);
        tokio::spawn(async move {
            supervisor
                .run_turn(item.agent_id, RunId::generate(), item)
                .await
        })
    }
    /// Every message the fake received (`client`) and sent (`server`), in order.
    fn log(&self) -> Vec<(String, Value)> {
        fs::read_to_string(self.root.join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                (v["dir"].as_str().unwrap().to_owned(), v["msg"].clone())
            })
            .collect()
    }
    fn client(&self, method: &str) -> Vec<Value> {
        self.log()
            .into_iter()
            .filter(|(dir, m)| dir == "client" && m["method"] == method)
            .map(|(_, m)| m)
            .collect()
    }
    fn deltas(&self) -> Vec<StreamDelta> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::AgentStream(s) => Some(s.delta),
                _ => None,
            })
            .collect()
    }
    fn states(&self) -> Vec<AgentState> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|(_, e)| match e {
                Event::AgentState(s) => Some(s.state),
                _ => None,
            })
            .collect()
    }
    async fn until(&self, what: impl Fn(&Self) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !what(self) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("condition before timeout");
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn start_initializes_and_starts_a_thread_in_the_agent_folder() {
    let s = Setup::new("", 0, None);
    let session = s.supervisor.start(&s.agent, None).await.unwrap();
    assert_eq!(session.session_id.0, RECORDED_THREAD);
    assert!(
        session
            .transcript_path
            .to_string_lossy()
            .ends_with(&format!("{RECORDED_THREAD}.jsonl"))
    );
    assert_eq!(
        s.store
            .main_session(s.agent.id)
            .unwrap()
            .unwrap()
            .session_id,
        session.session_id
    );
    let methods: Vec<_> = s
        .log()
        .into_iter()
        .filter(|(dir, _)| dir == "client")
        .map(|(_, m)| m["method"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(methods, ["initialize", "initialized", "thread/start"]);
    let params = &s.client("thread/start")[0]["params"];
    assert_eq!(params["approvalPolicy"], "untrusted");
    assert_eq!(params["approvalsReviewer"], "user");
    assert_eq!(params["sandbox"], "workspace-write");
    assert_eq!(params["cwd"], json!(s.agent.folder_path));
    assert_eq!(s.states(), [AgentState::Idle]);
    // Starting again while running returns the same session without a new process.
    let again = s.supervisor.start(&s.agent, None).await.unwrap();
    assert_eq!(again.session_id, session.session_id);
    assert_eq!(s.client("initialize").len(), 1);
    assert_eq!(
        s.supervisor.status().await.version.as_deref(),
        Some("codex-cli 0.0.0 (fake-codex)")
    );
    s.supervisor
        .stop(s.agent.id, StopMode::AfterTurn)
        .await
        .unwrap();
    assert_eq!(s.states().last(), Some(&AgentState::Stopped));
}

#[tokio::test]
async fn a_turn_streams_the_reply_and_ends_idle() {
    let s = Setup::new("turn", 1, None);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let result = s.spawn_turn("Reply with OK").await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.result.as_deref(), Some("OK"));
    let turn_start = &s.client("turn/start")[0]["params"];
    assert_eq!(turn_start["threadId"], RECORDED_THREAD);
    assert_eq!(turn_start["input"][0]["text"], "Reply with OK");
    let deltas = s.deltas();
    assert!(matches!(deltas[0], StreamDelta::TurnStarted { .. }));
    assert!(matches!(
        &deltas[1],
        StreamDelta::ItemCompleted { item: ConversationItem::UserMessage { text, .. } } if text == "Reply with OK"
    ));
    assert!(deltas.iter().any(|d| matches!(
        d,
        StreamDelta::ItemStarted {
            item: ConversationItem::AssistantText { .. }
        }
    )));
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, StreamDelta::TextDelta { text, .. } if text == "OK"))
    );
    assert!(deltas.iter().any(|d| matches!(d,
        StreamDelta::ItemCompleted { item: ConversationItem::AssistantText { text, .. } } if text == "OK")));
    assert!(matches!(
        deltas.last(),
        Some(StreamDelta::TurnEnded {
            outcome: TurnOutcome::Completed,
            ..
        })
    ));
    assert_eq!(
        s.states(),
        [AgentState::Idle, AgentState::Working, AgentState::Idle]
    );
    let page = s
        .supervisor
        .handle(CommandCall {
            caller: Caller::Owner,
            method: "conversation_page".into(),
            params: json!({"agent_id": s.agent.id, "session_id": null, "before": null, "limit": 10}),
        })
        .await
        .unwrap();
    let page: ConversationPageResult = serde_json::from_value(page).unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(matches!(&page.items[1], ConversationItem::AssistantText { text, .. } if text == "OK"));
    assert_eq!(page.stream_seq as usize, deltas.len());
}

#[tokio::test]
async fn a_second_message_waits_for_turn_completed() {
    let s = Setup::new("turn,turn", 5, None);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let first = s.spawn_turn("first");
    s.until(|s| s.client("turn/start").len() == 1).await;
    let second = s.spawn_turn("second");
    s.until(|s| {
        s.sink.events().iter().any(
            |(_, e)| matches!(e, Event::AgentQueue(q) if q.items.len() == 1 && q.items[0].text == "second"),
        )
    })
    .await;
    assert_eq!(
        first.await.unwrap().unwrap().outcome,
        TurnOutcome::Completed
    );
    assert_eq!(
        second.await.unwrap().unwrap().outcome,
        TurnOutcome::Completed
    );
    let log = s.log();
    let completed = log
        .iter()
        .position(|(dir, m)| dir == "server" && m["method"] == "turn/completed")
        .unwrap();
    let starts: Vec<_> = log
        .iter()
        .enumerate()
        .filter(|(_, (dir, m))| dir == "client" && m["method"] == "turn/start")
        .map(|(i, (_, m))| (i, m["params"]["input"][0]["text"].clone()))
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].1, "first");
    assert_eq!(starts[1].1, "second");
    assert!(starts[1].0 > completed);
}

#[tokio::test]
async fn interrupt_ends_the_turn_as_interrupted() {
    let s = Setup::new("interrupt", 20, None);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = s.spawn_turn("count");
    s.until(|s| {
        s.deltas()
            .iter()
            .any(|d| matches!(d, StreamDelta::TextDelta { .. }))
    })
    .await;
    s.supervisor.interrupt(s.agent.id).await.unwrap();
    let result = turn.await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Interrupted);
    let interrupt = &s.client("turn/interrupt")[0]["params"];
    assert_eq!(interrupt["threadId"], RECORDED_THREAD);
    assert_eq!(interrupt["turnId"], "01a1bed1-2b15-7141-a621-b0c49ba21d0d");
    assert_eq!(s.states().last(), Some(&AgentState::Idle));
    // No turn runs now.
    let err = s.supervisor.interrupt(s.agent.id).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

fn decisions(s: &Setup) -> Vec<Value> {
    s.log()
        .into_iter()
        .filter(|(dir, m)| dir == "client" && m.get("method").is_none())
        .map(|(_, m)| m["result"]["decision"].clone())
        .collect()
}

fn command_step(s: &Setup) -> Option<StepStatus> {
    s.deltas().iter().find_map(|d| match d {
        StreamDelta::ItemCompleted {
            item: ConversationItem::Step { step, .. },
        } if step.kind == StepKind::Command => Some(step.status),
        _ => None,
    })
}

#[tokio::test]
async fn an_allowed_command_is_accepted() {
    let gate = Arc::new(FakePermissionGate::allowing());
    let s = Setup::new("approval_accept", 1, Some(gate.clone()));
    s.supervisor.start(&s.agent, None).await.unwrap();
    let result = s.spawn_turn("touch").await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(decisions(&s), [json!("accept")]);
    let calls = gate.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].prompt.call.tool_name, "Bash");
    assert_eq!(
        calls[0].prompt.call.input["command"],
        "/bin/zsh -lc 'touch accepted.txt'"
    );
    assert_eq!(calls[0].prompt.agent_id, s.agent.id);
    assert_eq!(command_step(&s), Some(StepStatus::Done));
    assert!(s.states().contains(&AgentState::WaitingForApproval));
    assert_eq!(s.states().last(), Some(&AgentState::Idle));
}

#[tokio::test]
async fn a_denied_command_is_declined() {
    let gate = Arc::new(FakePermissionGate::denying("no"));
    let s = Setup::new("approval_decline", 1, Some(gate));
    s.supervisor.start(&s.agent, None).await.unwrap();
    let result = s.spawn_turn("touch").await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(decisions(&s), [json!("decline")]);
    assert_eq!(command_step(&s), Some(StepStatus::Denied));
}

#[tokio::test]
async fn without_a_gate_approvals_are_declined() {
    let s = Setup::new("approval_decline", 1, None);
    s.supervisor.start(&s.agent, None).await.unwrap();
    s.spawn_turn("touch").await.unwrap().unwrap();
    assert_eq!(decisions(&s), [json!("decline")]);
}

#[tokio::test]
async fn a_restart_resumes_the_stored_thread() {
    let s = Setup::new("turn,turn", 1, None);
    let first = s.supervisor.start(&s.agent, None).await.unwrap();
    s.spawn_turn("before").await.unwrap().unwrap();
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
    let second = s.supervisor.start(&s.agent, None).await.unwrap();
    assert_eq!(second.session_id, first.session_id);
    assert_eq!(second.transcript_path, first.transcript_path);
    let resume = &s.client("thread/resume")[0]["params"];
    assert_eq!(resume["threadId"], RECORDED_THREAD);
    assert_eq!(resume["approvalPolicy"], "untrusted");
    assert_eq!(resume["sandbox"], "workspace-write");
    assert_eq!(s.client("thread/start").len(), 1);
    let result = s.spawn_turn("after").await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    // The conversation keeps what this supervisor saw before the restart.
    let page = s
        .supervisor
        .conversation(s.agent.id, None, None, 10)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 4);
}

#[tokio::test]
async fn an_explicit_session_is_resumed() {
    let s = Setup::new("", 0, None);
    let session = s
        .supervisor
        .start(
            &s.agent,
            Some(SessionId("01a1bed0-0000-7000-8000-000000000000".into())),
        )
        .await
        .unwrap();
    assert_eq!(session.session_id.0, "01a1bed0-0000-7000-8000-000000000000");
    assert_eq!(
        s.client("thread/resume")[0]["params"]["threadId"],
        "01a1bed0-0000-7000-8000-000000000000"
    );
}

#[tokio::test]
async fn forks_and_claude_agents_are_unsupported() {
    let s = Setup::new("", 0, None);
    let mut claude = s.agent.clone();
    claude.harness = HarnessKind::ClaudeCode;
    let err = s.supervisor.start(&claude, None).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::HarnessUnsupported);
    let err = s
        .supervisor
        .run_fork(
            s.agent.id,
            ForkRun {
                run_id: RunId::generate(),
                from: SessionId(RECORDED_THREAD.into()),
                item: s.item("x"),
                tools: ForkTools::None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::HarnessUnsupported);
}

#[tokio::test]
async fn steer_sends_turn_steer_for_the_running_turn() {
    let s = Setup::new("interrupt", 20, None);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = s.spawn_turn("count");
    s.until(|s| {
        s.deltas()
            .iter()
            .any(|d| matches!(d, StreamDelta::TextDelta { .. }))
    })
    .await;
    s.supervisor
        .steer(s.agent.id, s.item("stop at 3"))
        .await
        .unwrap();
    let steer = &s.client("turn/steer")[0]["params"];
    assert_eq!(
        steer["expectedTurnId"],
        "01a1bed1-2b15-7141-a621-b0c49ba21d0d"
    );
    assert_eq!(steer["input"][0]["text"], "stop at 3");
    s.supervisor.interrupt(s.agent.id).await.unwrap();
    turn.await.unwrap().unwrap();
}
