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
use std::{collections::HashMap, fs, path::PathBuf, sync::Arc, time::Duration};

struct Setup {
    root: PathBuf,
    agent: Agent,
    sink: Arc<RecordingEventSink>,
    harness: ClaudeSupervisor,
}

impl Setup {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("formation-queue-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "queue".into(),
            name: "Queue".into(),
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
            created_at: std::time::SystemTime::now().into(),
            last_activity_at: None,
            detected: None,
        };
        let store = Arc::new(MemoryStore::default());
        store.insert_agent(&agent).unwrap();
        let sink = Arc::new(RecordingEventSink::default());
        let harness = ClaudeSupervisor::new(
            ClaudeConfig {
                binary: env!("CARGO_BIN_EXE_fake-claude").into(),
                config_dir: root.join("config"),
                env: HashMap::from([
                    (
                        "FAKE_CLAUDE_TRANSCRIPT".into(),
                        format!(
                            "{}/testkit/fixtures/claude/2-queue-later.stdout.jsonl",
                            env!("CARGO_MANIFEST_DIR")
                        ),
                    ),
                    ("FAKE_CLAUDE_LINE_DELAY_MS".into(), "10".into()),
                    ("FAKE_CLAUDE_ACCEPT_STEER".into(), "1".into()),
                    (
                        "FAKE_CLAUDE_INPUT_LOG".into(),
                        root.join("stdin.jsonl").display().to_string(),
                    ),
                ]),
                ..ClaudeConfig::default()
            },
            store,
            sink.clone(),
        );
        harness.start(&agent, None).await.unwrap();
        Self {
            root,
            agent,
            sink,
            harness,
        }
    }
    fn item(&self, text: &str) -> QueueItem {
        QueueItem {
            id: MessageId::generate(),
            agent_id: self.agent.id,
            source: QueueSource::Owner,
            text: text.into(),
            queued_at: std::time::SystemTime::now().into(),
        }
    }
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.harness
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
    }
    async fn send(&self, text: &str) -> QueueItem {
        serde_json::from_value(
            self.call(
                "agent_message_send",
                json!({"agent_id":self.agent.id,"text":text}),
            )
            .await
            .unwrap(),
        )
        .unwrap()
    }
    async fn wait(&self, predicate: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !predicate() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("queue operation before timeout");
    }
    fn inputs(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("stdin.jsonl"))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn queue(&self) -> Vec<QueueItem> {
        self.sink
            .events()
            .into_iter()
            .rev()
            .find_map(|(_, e)| match e {
                Event::AgentQueue(q) => Some(q.items),
                _ => None,
            })
            .unwrap_or_default()
    }
    fn ended(&self, id: MessageId) -> bool {
        self.sink.events().iter().any(|(_, e)| matches!(e, Event::AgentStream(e) if matches!(e.delta, StreamDelta::TurnEnded { message_id, .. } if message_id == id)))
    }
    async fn running(&self) -> QueueItem {
        let first = self.send("first").await;
        self.wait(|| !self.inputs().is_empty()).await;
        first
    }
    async fn stop(&self) {
        self.harness
            .stop(self.agent.id, StopMode::Now)
            .await
            .unwrap();
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn queued_steer_joins_running_turn_without_priority_or_extra_turn() {
    let s = Setup::new().await;
    for method in [
        "agent_message_steer",
        "agent_message_edit",
        "agent_message_withdraw",
    ] {
        assert!(s.harness.methods().contains(&method));
    }
    let first = s.running().await;
    let queued = s.item("steer now");
    let h = s.harness.clone();
    let item = queued.clone();
    let waiter =
        tokio::spawn(async move { h.run_turn(item.agent_id, RunId::generate(), item).await });
    s.wait(|| s.queue().iter().any(|p| p.id == queued.id)).await;
    s.call(
        "agent_message_steer",
        json!({"agent_id":s.agent.id,"message_id":queued.id}),
    )
    .await
    .unwrap();
    assert!(s.queue().is_empty());
    s.wait(|| s.inputs().len() == 2).await;
    let inputs = s.inputs();
    assert_eq!(inputs[0]["priority"], "later");
    assert_eq!(inputs[1]["message"]["content"], "steer now");
    assert!(inputs[1].get("priority").is_none());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .outcome,
        TurnOutcome::Completed
    );
    assert!(s.ended(first.id));
    let starts = s.sink.events().iter().filter(|(_, e)| matches!(e, Event::AgentStream(e) if matches!(e.delta, StreamDelta::TurnStarted { .. }))).count();
    assert_eq!(starts, 1);
    s.stop().await;
}

#[tokio::test]
async fn harness_steer_delivers_default_priority_and_idle_steer_conflicts() {
    let s = Setup::new().await;
    let first = s.running().await;
    s.harness
        .steer(s.agent.id, s.item("direct steer"))
        .await
        .unwrap();
    s.wait(|| s.inputs().len() == 2).await;
    assert_eq!(s.inputs()[1]["message"]["content"], "direct steer");
    assert!(s.inputs()[1].get("priority").is_none());
    s.wait(|| s.ended(first.id)).await;
    let idle = s.item("idle steer");
    assert_eq!(
        s.harness
            .steer(s.agent.id, idle.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        s.call(
            "agent_message_steer",
            json!({"agent_id":s.agent.id,"message_id":idle.id}),
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Conflict
    );
    assert_eq!(s.inputs().len(), 2);
    s.stop().await;
}

#[tokio::test]
async fn edit_updates_queue_event_response_and_dispatched_text() {
    let s = Setup::new().await;
    s.running().await;
    let queued = s.send("original").await;
    let edited = s
        .call(
            "agent_message_edit",
            json!({"agent_id":s.agent.id,"message_id":queued.id,"text":"edited"}),
        )
        .await
        .unwrap();
    assert_eq!(edited["text"], "edited");
    assert_eq!(s.queue()[0].text, "edited");
    assert_eq!(s.queue()[0].id, queued.id);
    s.wait(|| s.ended(queued.id)).await;
    assert_eq!(s.inputs()[1]["message"]["content"], "edited");
    s.stop().await;
}

#[tokio::test]
async fn withdraw_removes_message_and_resolves_waiting_caller_with_error() {
    let s = Setup::new().await;
    let first = s.running().await;
    let queued = s.item("withdraw me");
    let item = queued.clone();
    let h = s.harness.clone();
    let waiter =
        tokio::spawn(async move { h.run_turn(item.agent_id, RunId::generate(), item).await });
    s.wait(|| s.queue().iter().any(|p| p.id == queued.id)).await;
    s.call(
        "agent_message_withdraw",
        json!({"agent_id":s.agent.id,"message_id":queued.id}),
    )
    .await
    .unwrap();
    assert!(s.queue().is_empty());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    s.wait(|| s.ended(first.id)).await;
    assert_eq!(s.inputs().len(), 1);
    s.stop().await;
}

#[tokio::test]
async fn started_and_unknown_messages_return_clean_errors() {
    let s = Setup::new().await;
    let first = s.running().await;
    for method in ["agent_message_edit", "agent_message_withdraw"] {
        assert_eq!(
            s.call(
                method,
                json!({"agent_id":s.agent.id,"message_id":first.id,"text":"changed"})
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::Conflict
        );
        assert_eq!(
            s.call(
                method,
                json!({"agent_id":s.agent.id,"message_id":MessageId::generate(),"text":"changed"})
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::NotFound
        );
    }
    s.stop().await;
}
