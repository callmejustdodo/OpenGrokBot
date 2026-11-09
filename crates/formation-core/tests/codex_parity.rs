#![cfg(feature = "testkit")]
//! Issue #205: Codex agents report a usage limit like Claude's (#188) and record
//! their lifecycle in history (#196). `usage_limit.jsonl` is written from the
//! app-server schema, not recorded from a real limit.
use chrono::{DateTime, Local};
use formation_core::{
    harness::codex::{CodexConfig, CodexSupervisor},
    notify::{AttentionNotifier, monitor},
    ports::*,
    service::EventBus,
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{agent::*, common::*, events::Event, harness::*, request::*};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

fn agent(root: PathBuf) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "codex".into(),
        name: "Codex".into(),
        role: "developer".into(),
        harness: HarnessKind::Codex,
        folder_path: root,
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
    }
}

fn config(turns: &str) -> CodexConfig {
    CodexConfig {
        binary: env!("CARGO_BIN_EXE_fake-codex").into(),
        env: HashMap::from([("FAKE_CODEX_TURNS".into(), turns.into())]),
        ..CodexConfig::default()
    }
}

fn temp_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("formation-codex-parity-{}", RunId::generate()));
    fs::create_dir_all(&root).unwrap();
    root
}

#[tokio::test]
async fn usage_limit_names_the_limit_and_its_reset_time() {
    let root = temp_root();
    let agent = agent(root.clone());
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let bus = Arc::new(EventBus::new(256));
    let mut events = bus.subscribe();
    let notifier = Arc::new(AttentionNotifier::new(
        Arc::new(MemoryStore::default()),
        Arc::new(RecordingEventSink::default()),
    ));
    tokio::spawn(monitor(bus.subscribe(), notifier.clone()));
    let supervisor = CodexSupervisor::new(config("usage_limit"), store, bus.clone(), None);
    supervisor.start(&agent, None).await.unwrap();

    let failed = supervisor
        .run_turn(
            agent.id,
            RunId::generate(),
            QueueItem {
                id: MessageId::generate(),
                agent_id: agent.id,
                source: QueueSource::Owner,
                text: "hello".into(),
                queued_at: SystemTime::now().into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(failed.outcome, TurnOutcome::Failed);

    // The full primary window's reset time, in the Mac's time zone.
    let resets = DateTime::from_timestamp(1794219209, 0)
        .unwrap()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %:z");
    let reason = format!(
        "Codex usage limit: You've hit your usage limit. Try again later. · resets {resets} (usageLimitExceeded)"
    );
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap().event {
                Event::AgentState(e) if e.state == AgentState::Error => break e,
                _ => {}
            }
        }
    })
    .await
    .expect("agent.state error");
    assert_eq!(error.reason.as_deref(), Some(reason.as_str()));

    let item = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(item) = notifier.inbox().await.unwrap().pop() {
                break item;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("inbox item");
    assert_eq!(item.kind, AttentionKind::AgentError);
    assert_eq!(item.subject, reason);

    supervisor.stop(agent.id, StopMode::Now).await.unwrap();
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn start_then_stop_records_started_and_stopped() {
    let root = temp_root();
    let agent = agent(root.clone());
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let supervisor = CodexSupervisor::new(
        config(""),
        store.clone(),
        Arc::new(RecordingEventSink::default()),
        None,
    );
    supervisor.start(&agent, None).await.unwrap();
    supervisor.stop(agent.id, StopMode::Now).await.unwrap();

    let mut recorded: Vec<(ActivityKind, String)> = store
        .activity(Some(agent.id), None, None, 10)
        .unwrap()
        .into_iter()
        .map(|a| (a.kind, a.summary))
        .collect();
    recorded.sort_by_key(|(kind, _)| *kind != ActivityKind::AgentStarted);
    assert_eq!(
        recorded,
        [
            (ActivityKind::AgentStarted, "Codex".to_owned()),
            (ActivityKind::AgentStopped, "Codex".to_owned()),
        ]
    );
    let _ = fs::remove_dir_all(&root);
}
