#![cfg(feature = "testkit")]
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    testkit::{
        MemoryStore, RecordingEventSink,
        collab::{FakeRequestQueue, samples::work_request},
    },
};
use formation_proto::{agent::*, common::*, harness::*, request::*};
use serde_json::Value;
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
    queue: Arc<FakeRequestQueue>,
    supervisor: ClaudeSupervisor,
}
impl Setup {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("formation-requests-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "requests".into(),
            name: "Requests".into(),
            role: "developer".into(),
            harness: HarnessKind::ClaudeCode,
            folder_path: root.clone(),
            repositories: vec![root.join("repo")],
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
        let queue = Arc::new(FakeRequestQueue::new());
        let config = ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            env: HashMap::from([
                (
                    "FAKE_CLAUDE_TRANSCRIPT".into(),
                    format!(
                        "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                        env!("CARGO_MANIFEST_DIR")
                    ),
                ),
                ("FAKE_CLAUDE_LINE_DELAY_MS".into(), "10".into()),
                (
                    "FAKE_CLAUDE_LAUNCH_LOG".into(),
                    root.join("launches").display().to_string(),
                ),
                (
                    "FAKE_CLAUDE_INPUT_LOG".into(),
                    root.join("inputs").display().to_string(),
                ),
                ("FAKE_CLAUDE_WRITE_TRANSCRIPT".into(), "1".into()),
            ]),
            ..ClaudeConfig::default()
        };
        let supervisor = ClaudeSupervisor::with_request_queue(
            config,
            store.clone(),
            Arc::new(RecordingEventSink::default()),
            Some(queue.clone()),
        );
        Self {
            root,
            agent,
            store,
            queue,
            supervisor,
        }
    }
    fn owner(&self) -> QueueItem {
        QueueItem {
            id: MessageId::generate(),
            agent_id: self.agent.id,
            source: QueueSource::Owner,
            text: "owner turn".into(),
            queued_at: SystemTime::now().into(),
        }
    }
    fn launches(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("launches"))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter(|s| s.ends_with('\n'))
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
}
/// Whether `args` contains `flag` (`--permission-prompt-tool` on every gated run;
/// `--setting-sources` and `--permission-mode` only on a teammate's request fork).
fn has(args: &Value, flag: &str) -> bool {
    args.as_array().unwrap().contains(&Value::from(flag))
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn requests_wait_for_turn_then_fork_in_order_and_report_to_requester() {
    let s = Setup::new();
    let main = s.supervisor.start(&s.agent, None).await.unwrap();
    let turn = tokio::spawn({
        let supervisor = s.supervisor.clone();
        let item = s.owner();
        async move {
            supervisor
                .run_turn(item.agent_id, RunId::generate(), item)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while s
            .store
            .main_session(s.agent.id)
            .unwrap()
            .unwrap()
            .last_activity_at
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let mut updates = s.queue.subscribe_requests();
    let mut items = vec![];
    for (prompt, key) in [("first request", "one"), ("second request", "two")] {
        items.push(
            s.queue
                .enqueue_request(work_request(
                    MemberId::generate(),
                    MemberId::generate(),
                    s.agent.id,
                    prompt,
                    key,
                ))
                .await
                .unwrap(),
        );
    }
    assert!(s.queue.progress().is_empty());
    assert_eq!(s.launches().len(), 1);
    assert_eq!(turn.await.unwrap().unwrap().outcome, TurnOutcome::Completed);
    let original = fs::read(&main.transcript_path).unwrap();
    let fork_launch = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(launch) = s.launches().get(1) {
                break launch.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let run_id = serde_json::from_value(fork_launch["run_id"].clone()).unwrap();
    let context = s.supervisor.run(run_id).await.unwrap();
    assert_eq!(context.item, items[0]);
    assert_eq!(context.agent_id, s.agent.id);
    tokio::time::timeout(Duration::from_secs(10), async {
        while s.queue.progress().len() < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        s.queue.progress(),
        vec![
            (items[0].id, RunProgress::Started),
            (
                items[0].id,
                RunProgress::Completed {
                    result: Some("ONE-DONE".into())
                }
            ),
            (items[1].id, RunProgress::Started),
            (
                items[1].id,
                RunProgress::Completed {
                    result: Some("ONE-DONE".into())
                }
            ),
        ]
    );
    let statuses: Vec<_> = std::iter::from_fn(|| updates.try_recv().ok())
        .map(|r| (r.id, r.status))
        .collect();
    let ids: Vec<_> = items
        .iter()
        .map(|i| match i.source {
            QueueSource::Request { request_id, .. } => request_id,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        statuses,
        vec![
            (ids[0], RequestStatus::Queued),
            (ids[1], RequestStatus::Queued),
            (ids[0], RequestStatus::Running),
            (ids[0], RequestStatus::Completed),
            (ids[1], RequestStatus::Running),
            (ids[1], RequestStatus::Completed)
        ]
    );
    let launches = s.launches();
    assert_eq!(launches.len(), 3);
    assert_ne!(launches[1]["session"], launches[2]["session"]);
    let main_args = &launches[0]["args"];
    assert!(has(main_args, "--permission-prompt-tool"));
    // The main session and each gated fork can work in the agent's repositories.
    let add_dir = [
        Value::from("--add-dir"),
        Value::from(s.root.join("repo").display().to_string()),
    ];
    for launch in &launches {
        assert!(
            launch["args"]
                .as_array()
                .unwrap()
                .windows(2)
                .any(|w| w == add_dir)
        );
    }
    assert!(!has(main_args, "--setting-sources"));
    assert!(!has(main_args, "--permission-mode"));
    for launch in &launches[1..] {
        let args = &launch["args"];
        assert!(
            args.as_array()
                .unwrap()
                .windows(2)
                .any(|w| w == [Value::from("--setting-sources"), Value::from("")])
        );
        assert!(
            args.as_array()
                .unwrap()
                .windows(2)
                .any(|w| w == [Value::from("--permission-mode"), Value::from("default")])
        );
    }
    for launch in &launches[1..] {
        let args = launch["args"].as_array().unwrap();
        assert!(args.contains(&Value::from("--fork-session")));
        assert!(args.contains(&Value::from("--permission-prompt-tool")));
        assert!(args.contains(&Value::from("--settings")));
        assert!(args.contains(&Value::from(main.session_id.0.clone())));
        let run: RunId = serde_json::from_value(launch["run_id"].clone()).unwrap();
        let stored = s.store.fork_session(run).unwrap().unwrap();
        assert_eq!(stored.session_id.0, launch["session"].as_str().unwrap());
        assert!(!fs::read(&stored.transcript_path).unwrap().is_empty());
    }
    let inputs: Vec<Value> = fs::read_to_string(s.root.join("inputs"))
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    let texts: Vec<_> = inputs
        .iter()
        .map(|i| i["message"]["content"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["owner turn", "first request", "second request"]);
    assert_eq!(fs::read(&main.transcript_path).unwrap(), original);
    s.supervisor
        .stop(s.agent.id, StopMode::AfterTurn)
        .await
        .unwrap();
}

#[tokio::test]
async fn tool_free_fork_preserves_main_and_stores_result() {
    let s = Setup::new();
    let main = s.supervisor.start(&s.agent, None).await.unwrap();
    fs::create_dir_all(main.transcript_path.parent().unwrap()).unwrap();
    fs::write(&main.transcript_path, "original transcript\n").unwrap();
    let run_id = RunId::generate();
    let result = s
        .supervisor
        .run_fork(
            s.agent.id,
            ForkRun {
                run_id,
                from: main.session_id.clone(),
                item: s.owner(),
                tools: ForkTools::None,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.result.outcome, TurnOutcome::Completed);
    assert_eq!(s.store.fork_session(run_id).unwrap(), Some(result.session));
    assert_eq!(
        fs::read_to_string(&main.transcript_path).unwrap(),
        "original transcript\n"
    );
    let launch = s
        .launches()
        .into_iter()
        .find(|l| {
            l["args"]
                .as_array()
                .is_some_and(|args| args.contains(&Value::from("--fork-session")))
        })
        .unwrap();
    let args = launch["args"].as_array().unwrap();
    assert!(
        args.windows(2)
            .any(|w| w == [Value::from("--tools"), Value::from("")])
    );
    assert!(!args.contains(&Value::from("--permission-prompt-tool")));
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn schedule_fork_loads_the_owner_settings_and_keeps_the_gate() {
    let s = Setup::new();
    let main = s.supervisor.start(&s.agent, None).await.unwrap();
    let item = QueueItem {
        source: QueueSource::Schedule {
            schedule_id: ScheduleId::generate(),
        },
        ..s.owner()
    };
    let result = s
        .supervisor
        .run_fork(
            s.agent.id,
            ForkRun {
                run_id: RunId::generate(),
                from: main.session_id.clone(),
                item,
                tools: ForkTools::Gated,
            },
        )
        .await
        .unwrap();
    assert_eq!(result.result.outcome, TurnOutcome::Completed);
    let launches = s.launches();
    assert_eq!(launches.len(), 2);
    for launch in &launches {
        let args = &launch["args"];
        assert!(has(args, "--permission-prompt-tool"));
        assert!(has(args, "--mcp-config"));
        assert!(has(args, "--settings"));
        assert!(!has(args, "--setting-sources"));
        assert!(!has(args, "--permission-mode"));
    }
    assert_eq!(
        launches
            .iter()
            .filter(|l| has(&l["args"], "--fork-session"))
            .count(),
        1
    );
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}
