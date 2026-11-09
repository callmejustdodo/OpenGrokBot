#![cfg(feature = "testkit")]
//! Issue #185: a turn stopped by the usage limit says so, with Claude's reset time,
//! in `agent.state` and the inbox, and leaves Claude Code signed in. Its own test
//! binary, because the sign-in state is process-wide.
use formation_core::{
    agents::{ClaudeReadiness, HarnessReadiness},
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    notify::{AttentionNotifier, monitor},
    ports::*,
    service::EventBus,
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
const REASON: &str = "Claude usage limit: You've hit your 5-hour usage limit · resets 3:40pm (Asia/Seoul) (api_error)";

#[tokio::test]
async fn usage_limit_names_the_limit_and_keeps_the_harness_signed_in() {
    let root = std::env::temp_dir().join(format!("formation-usage-limit-{}", RunId::generate()));
    fs::create_dir_all(&root).unwrap();
    let agent = Agent {
        id: AgentId::generate(),
        slug: "limit".into(),
        name: "Limit".into(),
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
    let notifier = Arc::new(AttentionNotifier::new(
        Arc::new(MemoryStore::default()),
        Arc::new(RecordingEventSink::default()),
    ));
    tokio::spawn(monitor(bus.subscribe(), notifier.clone()));
    let supervisor = ClaudeSupervisor::new(
        ClaudeConfig {
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
                ("FAKE_CLAUDE_USAGE_LIMITS".into(), "1".into()),
            ]),
            ..ClaudeConfig::default()
        },
        store,
        bus.clone(),
    );
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

    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap().event {
                Event::AgentState(e) if e.state == AgentState::Error => break e,
                Event::HarnessUpdate(u) => panic!("usage limit changed sign-in: {u:?}"),
                _ => {}
            }
        }
    })
    .await
    .expect("agent.state error");
    assert_eq!(error.reason.as_deref(), Some(REASON));

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
    assert_eq!(item.subject, REASON);

    assert_eq!(supervisor.status().await.signed_in, Some(true));
    assert_eq!(supervisor.status().await.problem, None);
    let check = ClaudeReadiness::new(FAKE.into()).check().unwrap();
    assert_eq!(check[0].signed_in, Some(true));

    supervisor.stop(agent.id, StopMode::Now).await.unwrap();
    let _ = fs::remove_dir_all(&root);
}
