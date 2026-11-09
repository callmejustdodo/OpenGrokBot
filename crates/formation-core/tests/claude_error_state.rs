#![cfg(feature = "testkit")]
//! A failed turn leaves the agent in `error` in the snapshot, not only in events (#141).
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    service::{EventBus, SnapshotBuilder},
    testkit::{FakePeers, MemoryCollabStore, MemoryStore},
};
use formation_proto::{
    agent::*,
    common::*,
    events::Event,
    harness::TurnOutcome,
    policy::Caller,
    request::{QueueItem, QueueSource},
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    sync::Arc,
    time::{Duration, SystemTime},
};

#[tokio::test]
async fn failed_turn_keeps_error_in_snapshot_until_start() {
    let root = std::env::temp_dir().join(format!("formation-error-state-{}", RunId::generate()));
    fs::create_dir_all(&root).unwrap();
    // One turn that ends the way an API failure does; the process stays up.
    let transcript = root.join("failed.stdout.jsonl");
    fs::write(
        &transcript,
        json!({"type":"result","session_id":"s","subtype":"success","is_error":true,"terminal_reason":"api_error","result":"API Error: 500"}).to_string() + "\n",
    )
    .unwrap();
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
    let bus = Arc::new(EventBus::new(256));
    let mut events = bus.subscribe();
    let supervisor = Arc::new(ClaudeSupervisor::new(
        ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            env: HashMap::from([(
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                transcript.display().to_string(),
            )]),
            ..ClaudeConfig::default()
        },
        store.clone(),
        bus.clone(),
    ));
    let snapshots = SnapshotBuilder::new(
        store.clone(),
        Arc::new(MemoryCollabStore::new()),
        supervisor.clone(),
        Arc::new(FakePeers::new(MemberId::generate())),
    );
    let snapshot_state = || async {
        let (snapshot, _) = snapshots.capture(&bus).await.unwrap();
        snapshot.agents[0].state
    };
    let start = || {
        supervisor.handle(CommandCall {
            caller: Caller::Owner,
            method: "agent_start".into(),
            params: json!({ "agent_id": agent.id }),
        })
    };

    start().await.unwrap();
    assert_eq!(snapshot_state().await, AgentState::Idle);

    let item = QueueItem {
        id: MessageId::generate(),
        agent_id: agent.id,
        source: QueueSource::Owner,
        text: "hello".into(),
        queued_at: SystemTime::now().into(),
    };
    let turn = supervisor
        .run_turn(agent.id, RunId::generate(), item)
        .await
        .unwrap();
    assert_eq!(turn.outcome, TurnOutcome::Failed);
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Event::AgentState(e) = events.recv().await.unwrap().event
                && e.state == AgentState::Error
            {
                break e;
            }
        }
    })
    .await
    .expect("agent.state error");
    assert_eq!(error.reason.as_deref(), Some("API Error: 500 (api_error)"));
    // The snapshot matches the last event, and keeps it while the process lives.
    assert_eq!(snapshot_state().await, AgentState::Error);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(snapshot_state().await, AgentState::Error);

    // Starting the agent again clears the error.
    let started = start().await.unwrap();
    assert_eq!(started["state"], json!("idle"));
    assert_eq!(snapshot_state().await, AgentState::Idle);

    supervisor
        .stop(agent.id, StopMode::AfterTurn)
        .await
        .unwrap();
    assert_eq!(snapshot_state().await, AgentState::Stopped);
    let _ = fs::remove_dir_all(&root);
}
