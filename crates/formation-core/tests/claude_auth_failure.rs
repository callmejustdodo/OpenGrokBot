#![cfg(feature = "testkit")]
//! Issue #140: a turn that fails to authenticate reports Claude Code as signed out
//! until a turn succeeds. Its own test binary, because the reported failure is
//! process-wide.
use formation_core::{
    agents::{ClaudeReadiness, HarnessReadiness},
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{agent::*, common::*, events::Event, harness::*, request::*};
use std::{
    collections::HashMap,
    fs,
    sync::Arc,
    time::{Duration, SystemTime},
};

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");

fn updates(sink: &RecordingEventSink) -> Vec<HarnessStatus> {
    sink.events()
        .into_iter()
        .filter_map(|(_, e)| match e {
            Event::HarnessUpdate(u) => Some(u.harnesses[0].clone()),
            _ => None,
        })
        .collect()
}

async fn until(mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("condition before timeout");
}

#[tokio::test]
async fn auth_failure_reports_signed_out_until_a_turn_succeeds() {
    let root = std::env::temp_dir().join(format!("formation-auth-{}", RunId::generate()));
    fs::create_dir_all(&root).unwrap();
    let agent = Agent {
        id: AgentId::generate(),
        slug: "auth".into(),
        name: "Auth".into(),
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
    let config = ClaudeConfig {
        binary: FAKE.into(),
        config_dir: root.join("config"),
        env: HashMap::from([
            (
                "FAKE_CLAUDE_TRANSCRIPT".to_string(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            ),
            ("FAKE_CLAUDE_AUTH_FAILURES".into(), "1".into()),
        ]),
        ..ClaudeConfig::default()
    };
    let sink = Arc::new(RecordingEventSink::default());
    let supervisor = ClaudeSupervisor::new(config, store, sink.clone());
    supervisor.start(&agent, None).await.unwrap();
    let item = |text: &str| QueueItem {
        id: MessageId::generate(),
        agent_id: agent.id,
        source: QueueSource::Owner,
        text: text.into(),
        queued_at: SystemTime::now().into(),
    };

    // The first turn fails to authenticate, as in the IC-1 run.
    let failed = supervisor
        .run_turn(agent.id, RunId::generate(), item("first"))
        .await
        .unwrap();
    assert_eq!(failed.outcome, TurnOutcome::Failed);
    assert!(sink.events().iter().any(|(_, e)| matches!(e,
        Event::AgentState(s) if s.state == AgentState::Error && s.reason.as_deref() == Some("Failed to authenticate: OAuth session expired and could not be refreshed (api_error)"))));
    until(|| !updates(&sink).is_empty()).await;
    let update = &updates(&sink)[0];
    assert_eq!(update.signed_in, Some(false));
    assert!(
        update
            .problem
            .as_deref()
            .unwrap()
            .contains("claude auth login")
    );
    // `claude auth status` still says signed in; both probes report the failure.
    let status = supervisor.status().await;
    assert_eq!(status.signed_in, Some(false));
    assert_eq!(status.problem, update.problem);
    let check = ClaudeReadiness::new(FAKE.into()).check().unwrap();
    assert_eq!(check[0].signed_in, Some(false));
    assert_eq!(check[0].problem, update.problem);

    // A successful turn clears it and publishes again.
    let done = supervisor
        .run_turn(agent.id, RunId::generate(), item("second"))
        .await
        .unwrap();
    assert_eq!(done.outcome, TurnOutcome::Completed);
    until(|| updates(&sink).len() == 2).await;
    let update = &updates(&sink)[1];
    assert_eq!(update.signed_in, Some(true));
    assert_eq!(update.problem, None);
    assert_eq!(supervisor.status().await.signed_in, Some(true));
    let check = ClaudeReadiness::new(FAKE.into()).check().unwrap();
    assert_eq!(check[0].signed_in, Some(true));
    assert_eq!(check[0].problem, None);

    supervisor.stop(agent.id, StopMode::Now).await.unwrap();
    let _ = fs::remove_dir_all(&root);
}
