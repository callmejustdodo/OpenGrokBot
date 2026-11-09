#![cfg(feature = "testkit")]
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    testkit::{MemoryStore, RecordingEventSink},
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

struct Setup {
    root: PathBuf,
    agent: Agent,
    store: Arc<MemoryStore>,
    sink: Arc<RecordingEventSink>,
    supervisor: ClaudeSupervisor,
}
impl Setup {
    fn new(fixture: &str, delay: u64, stalled: u64) -> Self {
        let root = std::env::temp_dir().join(format!("formation-supervisor-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "test".into(),
            name: "Test".into(),
            role: "developer".into(),
            harness: HarnessKind::ClaudeCode,
            folder_path: root.clone(),
            repositories: vec![],
            avatar: Avatar {
                shape: "circle".into(),
                color: "blue".into(),
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
        let config = ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            stalled_after: Duration::from_millis(stalled),
            env: HashMap::from([
                (
                    "FAKE_CLAUDE_TRANSCRIPT".into(),
                    format!(
                        "{}/testkit/fixtures/claude/{fixture}.stdout.jsonl",
                        env!("CARGO_MANIFEST_DIR")
                    ),
                ),
                ("FAKE_CLAUDE_LINE_DELAY_MS".into(), delay.to_string()),
            ]),
            ..ClaudeConfig::default()
        };
        let supervisor = ClaudeSupervisor::new(config, store.clone(), sink.clone());
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
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.supervisor
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
    }
    async fn state(&self, state: AgentState) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .sink
                    .events()
                    .iter()
                    .any(|(_, e)| matches!(e,Event::AgentState(s) if s.state==state))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("state event before timeout");
    }
    async fn output(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if self
                    .store
                    .main_session(self.agent.id)
                    .unwrap()
                    .is_some_and(|s| s.last_activity_at.is_some())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
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
}
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn one_turn_streams_to_result_and_idle() {
    let s = Setup::new("1-one-turn", 1, 600_000);
    let session = s.supervisor.start(&s.agent, None).await.unwrap();
    let mut stream = s.supervisor.subscribe();
    let item = s.item("one");
    let message_id = item.id;
    let run = RunId::generate();
    let h = s.supervisor.clone();
    let id = s.agent.id;
    let task = tokio::spawn(async move { h.run_turn(id, run, item).await });
    s.output().await;
    assert_eq!(s.supervisor.run(run).await.unwrap().item.id, message_id);
    let result = task.await.unwrap().unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.result.as_deref(), Some("ONE-DONE"));
    assert!(s.supervisor.run(run).await.is_none());
    let mut events = vec![];
    while let Ok(event) = stream.try_recv() {
        events.push(event);
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e.delta, StreamDelta::TextDelta { .. }))
    );
    assert!(events.iter().any(|e| matches!(
        e.delta,
        StreamDelta::TurnEnded {
            outcome: TurnOutcome::Completed,
            ..
        }
    )));
    assert!(events.iter().all(|e| e.session_id == session.session_id));
    assert!(
        events
            .windows(2)
            .all(|w| w[0].stream_seq + 1 == w[1].stream_seq)
    );
    let states: Vec<_> = s
        .sink
        .events()
        .iter()
        .filter_map(|(_, e)| {
            if let Event::AgentState(s) = e {
                Some(s.state)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        states,
        vec![AgentState::Idle, AgentState::Working, AgentState::Idle]
    );
    s.supervisor.stop(id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn queue_is_fifo_and_second_turn_waits() {
    let s = Setup::new("2-queue-later", 2, 600_000);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let first = s.spawn_turn("first");
    s.output().await;
    let second = s.spawn_turn("second");
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!first.is_finished());
    assert!(!second.is_finished());
    let queues: Vec<_> = s
        .sink
        .events()
        .into_iter()
        .filter_map(|(_, e)| {
            if let Event::AgentQueue(q) = e {
                Some(q.items)
            } else {
                None
            }
        })
        .collect();
    assert!(queues.iter().any(|q| q.len() == 1 && q[0].text == "second"));
    assert_eq!(
        first.await.unwrap().unwrap().outcome,
        TurnOutcome::Completed
    );
    assert_eq!(
        second.await.unwrap().unwrap().outcome,
        TurnOutcome::Completed
    );
    let events = s.sink.events();
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::AgentStream(e) = e {
                match e.delta {
                    StreamDelta::TurnStarted { .. } => Some("start"),
                    StreamDelta::TurnEnded { .. } => Some("end"),
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(deltas, vec!["start", "end", "start", "end"]);
    s.supervisor
        .stop(s.agent.id, StopMode::AfterTurn)
        .await
        .unwrap();
}

#[tokio::test]
async fn stop_now_waits_for_interrupt_result_and_cancels_queue() {
    let s = Setup::new("1-one-turn", 30, 600_000);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = s.spawn_turn("one");
    s.output().await;
    let queued = s.spawn_turn("cancel me");
    tokio::task::yield_now().await;
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
    assert_eq!(
        turn.await.unwrap().unwrap().outcome,
        TurnOutcome::Interrupted
    );
    assert_eq!(queued.await.unwrap().unwrap_err().code, ErrorCode::Conflict);
    s.state(AgentState::Stopped).await;
    let events = s.sink.events();
    let end=events.iter().position(|(_,e)|matches!(e,Event::AgentStream(e) if matches!(e.delta,StreamDelta::TurnEnded{outcome:TurnOutcome::Interrupted,..}))).unwrap();
    let stopped = events
        .iter()
        .position(|(_, e)| matches!(e,Event::AgentState(e) if e.state==AgentState::Stopped))
        .unwrap();
    assert!(end < stopped);
}

#[tokio::test]
async fn stop_after_turn_preserves_completion() {
    let s = Setup::new("1-one-turn", 2, 600_000);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = s.spawn_turn("one");
    s.output().await;
    s.supervisor
        .stop(s.agent.id, StopMode::AfterTurn)
        .await
        .unwrap();
    assert_eq!(
        turn.await.unwrap().unwrap().result.as_deref(),
        Some("ONE-DONE")
    );
    s.state(AgentState::Stopped).await;
}

#[tokio::test]
async fn stalled_resumes_when_output_returns() {
    let s = Setup::new("1-one-turn", 15, 5);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = s.spawn_turn("one");
    s.state(AgentState::Stalled).await;
    assert_eq!(turn.await.unwrap().unwrap().outcome, TurnOutcome::Completed);
    let states: Vec<_> = s
        .sink
        .events()
        .iter()
        .filter_map(|(_, e)| {
            if let Event::AgentState(s) = e {
                Some(s.state)
            } else {
                None
            }
        })
        .collect();
    assert!(
        states
            .windows(2)
            .any(|w| w == [AgentState::Stalled, AgentState::Working])
    );
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn every_served_command_and_turn_interrupt_keeps_process() {
    let s = Setup::new("2-queue-later", 3, 600_000);
    let id = s.agent.id;
    assert_eq!(s.supervisor.methods().len(), 9);
    let started = s.call("agent_start", json!({"agent_id":id})).await.unwrap();
    assert_eq!(started["state"], "idle");
    let first = s
        .call("agent_message_send", json!({"agent_id":id,"text":"first"}))
        .await
        .unwrap();
    assert_eq!(first["text"], "first");
    s.output().await;
    s.call("agent_turn_interrupt", json!({"agent_id":id}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        if s.sink.events().iter().any(|(_,e)|matches!(e,Event::AgentStream(e) if matches!(e.delta,StreamDelta::TurnEnded{outcome:TurnOutcome::Interrupted,..}))){break;}
        tokio::time::sleep(Duration::from_millis(1)).await;
    }}).await.unwrap();
    s.call("agent_message_send", json!({"agent_id":id,"text":"second"}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {loop {
        if s.sink.events().iter().any(|(_,e)|matches!(e,Event::AgentStream(e) if matches!(e.delta,StreamDelta::TurnEnded{outcome:TurnOutcome::Completed,..}))){break;}
        tokio::time::sleep(Duration::from_millis(1)).await;
    }}).await.unwrap();
    let page = s
        .call("conversation_page", json!({"agent_id":id,"limit":10}))
        .await
        .unwrap();
    assert_eq!(page["items"], json!([]));
    let stopped = s
        .call("agent_stop", json!({"agent_id":id,"mode":"after_turn"}))
        .await
        .unwrap();
    assert_eq!(stopped["state"], "stopped");
    for method in s.supervisor.methods() {
        assert_eq!(
            s.call(method, json!({"bad":true})).await.unwrap_err().code,
            ErrorCode::InvalidParams
        );
    }
}

#[tokio::test]
async fn restart_uses_stored_session_and_launches_gate_flags_in_agent_folder() {
    use std::os::unix::fs::PermissionsExt;
    let s = Setup::new("1-one-turn", 0, 600_000);
    let script = s.root.join("launcher");
    fs::write(&script,"#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE_ARGS\"\npwd > \"$CAPTURE_CWD\"\nexec \"$REAL_FAKE\" \"$@\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let config = ClaudeConfig {
        binary: script,
        config_dir: s.root.join("cfg"),
        pre_tool_use_command: "custom gate hook".into(),
        mcp_config: "custom gate mcp".into(),
        env: HashMap::from([
            (
                "CAPTURE_ARGS".into(),
                s.root.join("args").display().to_string(),
            ),
            (
                "CAPTURE_CWD".into(),
                s.root.join("cwd").display().to_string(),
            ),
            ("REAL_FAKE".into(), env!("CARGO_BIN_EXE_fake-claude").into()),
            (
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            ),
        ]),
        ..ClaudeConfig::default()
    };
    let first = ClaudeSupervisor::new(config.clone(), s.store.clone(), s.sink.clone());
    let session = first.start(&s.agent, None).await.unwrap();
    first
        .run_turn(s.agent.id, RunId::generate(), s.item("one"))
        .await
        .unwrap();
    first.stop(s.agent.id, StopMode::Now).await.unwrap();
    let second = ClaudeSupervisor::new(config, s.store.clone(), s.sink.clone());
    assert_eq!(
        second.start(&s.agent, None).await.unwrap().session_id,
        session.session_id
    );
    second
        .run_turn(s.agent.id, RunId::generate(), s.item("two"))
        .await
        .unwrap();
    second.stop(s.agent.id, StopMode::Now).await.unwrap();
    let args = fs::read_to_string(s.root.join("args")).unwrap();
    assert!(args.contains(&format!("--resume\n{}\n", session.session_id.0)));
    assert!(!args.contains("--session-id"));
    assert!(args.contains("--permission-prompt-tool\nmcp__formation__permission\n"));
    assert!(args.contains("--mcp-config\ncustom gate mcp\n"));
    assert!(args.contains("PreToolUse"));
    assert!(args.contains("custom gate hook"));
    assert_eq!(
        fs::read_to_string(s.root.join("cwd")).unwrap().trim(),
        fs::canonicalize(&s.root).unwrap().to_str().unwrap()
    );
}

#[tokio::test]
async fn conversation_reads_transcript_steps_and_paginates() {
    let s = Setup::new("1-one-turn", 0, 600_000);
    let session = s.supervisor.start(&s.agent, None).await.unwrap();
    fs::create_dir_all(session.transcript_path.parent().unwrap()).unwrap();
    let records = [
        json!({"type":"user","uuid":"u1","timestamp":"2026-11-09T00:00:00Z","message":{"content":"hello"}}),
        json!({"type":"assistant","uuid":"a1","timestamp":"2026-11-09T00:00:01Z","message":{"id":"msg1","content":[{"type":"text","text":"hello back"},{"type":"tool_use","id":"tool1","name":"Bash","input":{"command":"ls"}}]}}),
        json!({"type":"user","uuid":"u2","timestamp":"2026-11-09T00:00:02Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool1","content":"file.txt"}]}}),
    ];
    fs::write(
        &session.transcript_path,
        records.iter().map(|r| format!("{r}\n")).collect::<String>() + "{\"type\":",
    )
    .unwrap();
    let page = s
        .supervisor
        .conversation(s.agent.id, None, None, 1)
        .await
        .unwrap();
    assert!(page.has_more);
    assert!(
        matches!(&page.items[0],ConversationItem::Step{step,..} if step.status==StepStatus::Done && step.target=="ls" && step.detail.as_deref()==Some("file.txt"))
    );
    let page = s
        .supervisor
        .conversation(s.agent.id, None, Some(ItemId("tool1".into())), 10)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(!page.has_more);
    assert_eq!(
        s.supervisor
            .conversation(s.agent.id, None, Some(ItemId("missing".into())), 10)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}
