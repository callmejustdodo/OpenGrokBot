#![cfg(feature = "testkit")]
//! Issue #113: stop now always ends, sign-in is reported, the owner's queue survives
//! a restart, and `conversation_page.stream_seq` is a usable watermark.
use formation_core::{
    agents::{ClaudeReadiness, HarnessReadiness},
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    testkit::{
        MemoryStore, RecordingEventSink,
        collab::{FakeRequestQueue, samples::work_request},
    },
};
use formation_proto::{agent::*, common::*, events::Event, harness::*, policy::Caller, request::*};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

const FAKE: &str = env!("CARGO_BIN_EXE_fake-claude");

struct Setup {
    root: PathBuf,
    agent: Agent,
    store: Arc<MemoryStore>,
    config: ClaudeConfig,
}
impl Setup {
    fn new(delay: u64, extra: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("formation-hardening-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "hardening".into(),
            name: "Hardening".into(),
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
        };
        let store = Arc::new(MemoryStore::default());
        store.insert_agent(&agent).unwrap();
        let mut env = HashMap::from([
            (
                "FAKE_CLAUDE_TRANSCRIPT".to_string(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            ),
            ("FAKE_CLAUDE_LINE_DELAY_MS".into(), delay.to_string()),
            ("FAKE_CLAUDE_WRITE_TRANSCRIPT".into(), "1".into()),
        ]);
        env.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let config = ClaudeConfig {
            binary: FAKE.into(),
            config_dir: root.join("config"),
            stop_timeout: Duration::from_millis(300),
            env,
            ..ClaudeConfig::default()
        };
        Self {
            root,
            agent,
            store,
            config,
        }
    }
    fn supervisor(
        &self,
        sink: Arc<RecordingEventSink>,
        requests: Option<Arc<dyn RequestQueue>>,
    ) -> ClaudeSupervisor {
        ClaudeSupervisor::with_request_queue(
            self.config.clone(),
            self.store.clone(),
            sink,
            requests,
        )
    }
    fn owner(&self, text: &str) -> QueueItem {
        QueueItem {
            id: MessageId::generate(),
            agent_id: self.agent.id,
            source: QueueSource::Owner,
            text: text.into(),
            queued_at: SystemTime::now().into(),
        }
    }
    async fn until(&self, mut done: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("condition before timeout");
    }
    async fn output(&self) {
        self.until(|| {
            self.store
                .main_session(self.agent.id)
                .unwrap()
                .is_some_and(|s| s.last_activity_at.is_some())
        })
        .await;
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stopped(sink: &RecordingEventSink) -> bool {
    sink.events()
        .iter()
        .any(|(_, e)| matches!(e, Event::AgentState(s) if s.state == AgentState::Stopped))
}

#[tokio::test]
async fn stop_now_kills_a_turn_that_ignores_the_interrupt() {
    // One line a minute: the turn would not end on its own during the test.
    let s = Setup::new(60_000, &[("FAKE_CLAUDE_IGNORE_INTERRUPT", "1")]);
    let sink = Arc::new(RecordingEventSink::default());
    let supervisor = s.supervisor(sink.clone(), None);
    supervisor.start(&s.agent, None).await.unwrap();
    let turn = tokio::spawn({
        let supervisor = supervisor.clone();
        let item = s.owner("never ends");
        async move {
            supervisor
                .run_turn(item.agent_id, RunId::generate(), item)
                .await
        }
    });
    s.output().await;
    let began = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(5),
        supervisor.stop(s.agent.id, StopMode::Now),
    )
    .await
    .expect("stop now ends")
    .unwrap();
    assert!(began.elapsed() >= Duration::from_millis(300));
    assert!(turn.await.unwrap().is_err());
    assert!(stopped(&sink));
    // The supervisor reaped the process, so the agent can start again.
    supervisor.start(&s.agent, None).await.unwrap();
    supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn stop_now_kills_a_running_request_fork() {
    let s = Setup::new(60_000, &[]);
    let sink = Arc::new(RecordingEventSink::default());
    let queue = Arc::new(FakeRequestQueue::new());
    let supervisor = s.supervisor(sink.clone(), Some(queue.clone()));
    supervisor.start(&s.agent, None).await.unwrap();
    let item = queue
        .enqueue_request(work_request(
            MemberId::generate(),
            MemberId::generate(),
            s.agent.id,
            "slow request",
            "slow",
        ))
        .await
        .unwrap();
    s.until(|| queue.progress() == vec![(item.id, RunProgress::Started)])
        .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        supervisor.stop(s.agent.id, StopMode::Now),
    )
    .await
    .expect("stop now ends")
    .unwrap();
    assert_eq!(
        queue.progress(),
        vec![
            (item.id, RunProgress::Started),
            (item.id, RunProgress::Interrupted)
        ]
    );
    assert!(stopped(&sink));
}

#[tokio::test]
async fn status_and_harness_check_report_sign_in() {
    let s = Setup::new(0, &[]);
    let supervisor = s.supervisor(Arc::new(RecordingEventSink::default()), None);
    let status = supervisor.status().await;
    assert!(status.installed);
    assert_eq!(status.signed_in, Some(true));
    assert_eq!(status.version.as_deref(), Some("0.0.0 (fake-claude)"));
    assert_eq!(status.problem, None);
    // `harness_check` uses the same probe.
    assert_eq!(
        ClaudeReadiness::new(FAKE.into()).check().unwrap()[0].signed_in,
        Some(true)
    );

    let signed_out = s.root.join("signed-out-claude");
    fs::write(
        &signed_out,
        format!("#!/bin/sh\nFAKE_CLAUDE_SIGNED_OUT=1 exec '{FAKE}' \"$@\"\n"),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&signed_out, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = ClaudeConfig {
        binary: signed_out.clone(),
        ..s.config.clone()
    };
    let supervisor = ClaudeSupervisor::new(
        config,
        s.store.clone(),
        Arc::new(RecordingEventSink::default()),
    );
    let status = supervisor.status().await;
    assert!(status.installed);
    assert_eq!(status.signed_in, Some(false));
    assert!(status.problem.is_some());
    assert_eq!(
        ClaudeReadiness::new(signed_out).check().unwrap()[0].signed_in,
        Some(false)
    );
}

#[tokio::test]
async fn owner_queue_is_delivered_after_a_restart() {
    let s = Setup::new(20, &[]);
    let first_sink = Arc::new(RecordingEventSink::default());
    let first = s.supervisor(first_sink.clone(), None);
    first.start(&s.agent, None).await.unwrap();
    let send = |text: &'static str| {
        let first = first.clone();
        let agent_id = s.agent.id;
        async move {
            let value = first
                .handle(CommandCall {
                    caller: Caller::Owner,
                    method: "agent_message_send".into(),
                    params: json!({"agent_id": agent_id, "text": text}),
                })
                .await
                .unwrap();
            serde_json::from_value::<QueueItem>(value).unwrap()
        }
    };
    send("runs now").await;
    s.output().await;
    let waiting = send("waits for the restart").await;
    s.until(|| s.store.owner_queue(s.agent.id).unwrap() == vec![waiting.clone()])
        .await;
    // formationd stops every agent with "stop now" when it shuts down.
    first.stop(s.agent.id, StopMode::Now).await.unwrap();
    assert_eq!(
        s.store.owner_queue(s.agent.id).unwrap(),
        vec![waiting.clone()]
    );

    let sink = Arc::new(RecordingEventSink::default());
    let second = s.supervisor(sink.clone(), None);
    second.start(&s.agent, None).await.unwrap();
    s.until(|| {
        sink.events().iter().any(|(_, e)| {
            matches!(e, Event::AgentStream(e) if e.delta == StreamDelta::TurnEnded {
                message_id: waiting.id,
                outcome: TurnOutcome::Completed,
            })
        })
    })
    .await;
    assert!(s.store.owner_queue(s.agent.id).unwrap().is_empty());
    second.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn conversation_page_stream_seq_marks_items_in_the_transcript() {
    let s = Setup::new(0, &[]);
    let sink = Arc::new(RecordingEventSink::default());
    let supervisor = s.supervisor(sink.clone(), None);
    supervisor.start(&s.agent, None).await.unwrap();
    let page = supervisor
        .conversation(s.agent.id, None, None, 50)
        .await
        .unwrap();
    assert_eq!(page.stream_seq, 0);
    let item = s.owner("one");
    let result = supervisor
        .run_turn(s.agent.id, RunId::generate(), item.clone())
        .await
        .unwrap();
    assert_eq!(result.outcome, TurnOutcome::Completed);
    let ended = sink
        .events()
        .iter()
        .find_map(|(_, e)| match e {
            Event::AgentStream(e)
                if matches!(e.delta, StreamDelta::TurnEnded { message_id, .. } if message_id == item.id) =>
            {
                Some(e.stream_seq)
            }
            _ => None,
        })
        .unwrap();
    let page = supervisor
        .conversation(s.agent.id, None, None, 50)
        .await
        .unwrap();
    assert_eq!(page.stream_seq, ended);
    assert!(!page.items.is_empty());
    supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}
