#![cfg(feature = "testkit")]

//! A steer that Claude answers with its own result must not end the next queued turn (#172).

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
    request::{QueueItem, QueueSource},
};
use std::{collections::HashMap, fs, sync::Arc, time::Duration};

#[tokio::test]
async fn separately_answered_steer_does_not_end_the_next_queued_turn() {
    let root = std::env::temp_dir().join(format!("formation-steer-{}", RunId::generate()));
    fs::create_dir_all(&root).unwrap();
    let agent = Agent {
        id: AgentId::generate(),
        slug: "steer".into(),
        name: "Steer".into(),
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
        running_requests: vec![],
    };
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let sink = Arc::new(RecordingEventSink::default());
    let input_log = root.join("stdin.jsonl");
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
                ("FAKE_CLAUDE_SEPARATE_STEER".into(), "1".into()),
                (
                    "FAKE_CLAUDE_INPUT_LOG".into(),
                    input_log.display().to_string(),
                ),
            ]),
            ..ClaudeConfig::default()
        },
        store,
        sink.clone(),
    );
    harness.start(&agent, None).await.unwrap();
    let item = |text: &str| QueueItem {
        id: MessageId::generate(),
        agent_id: agent.id,
        source: QueueSource::Owner,
        text: text.into(),
        queued_at: std::time::SystemTime::now().into(),
    };
    let (first, second) = (item("first"), item("second"));
    let run = |item: QueueItem| {
        let h = harness.clone();
        tokio::spawn(async move { h.run_turn(item.agent_id, RunId::generate(), item).await })
    };
    let first_done = run(first.clone());
    // The first turn is running, and the second waits in the queue.
    tokio::time::timeout(Duration::from_secs(5), async {
        while fs::read_to_string(&input_log)
            .unwrap_or_default()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let second_done = run(second.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !sink.events().iter().any(
            |(_, e)| matches!(e, Event::AgentQueue(q) if q.items.iter().any(|i| i.id == second.id)),
        ) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    harness.steer(agent.id, item("steer")).await.unwrap();

    let wait = |task: tokio::task::JoinHandle<Result<TurnResult>>| async {
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    };
    let first_result = wait(first_done).await;
    let second_result = wait(second_done).await;
    // The steer's reply belongs to the turn it steered; the queued turn gets its own.
    assert_eq!(first_result.result.as_deref(), Some("STEERED"));
    assert_eq!(second_result.outcome, TurnOutcome::Completed);
    assert_eq!(second_result.result.as_deref(), Some("SECOND-DONE"));

    // Every assistant text lies inside one turn's framing, in order.
    let mut open = None;
    let mut frames = Vec::new();
    let mut texts = Vec::new();
    for (_, event) in sink.events() {
        let Event::AgentStream(e) = event else {
            continue;
        };
        match e.delta {
            StreamDelta::TurnStarted { message_id } => {
                assert_eq!(open, None, "turn started inside another turn");
                open = Some(message_id);
                frames.push(("started", message_id));
            }
            StreamDelta::TurnEnded { message_id, .. } => {
                assert_eq!(open, Some(message_id), "turn ended out of frame");
                open = None;
                frames.push(("ended", message_id));
            }
            StreamDelta::ItemCompleted {
                item: ConversationItem::AssistantText { text, .. },
            } => texts.push((open, text)),
            _ => {}
        }
    }
    assert_eq!(
        frames,
        [
            ("started", first.id),
            ("ended", first.id),
            ("started", second.id),
            ("ended", second.id),
        ]
    );
    assert!(texts.contains(&(Some(first.id), "STEERED".into())));
    assert!(texts.contains(&(Some(second.id), "SECOND-DONE".into())));
    assert!(texts.iter().all(|(turn, _)| turn.is_some()));

    harness.stop(agent.id, StopMode::Now).await.unwrap();
    let _ = fs::remove_dir_all(&root);
}
