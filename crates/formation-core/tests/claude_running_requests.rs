#![cfg(feature = "testkit")]
//! Teammates' requests running in forks show on the owner's agent (#35).
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    testkit::{
        MemoryStore, RecordingEventSink,
        collab::{FakeRequestQueue, samples::work_request},
    },
};
use formation_proto::{agent::*, common::*, events::Event, policy::Caller, request::*};
use serde_json::json;
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
    queue: Arc<FakeRequestQueue>,
    supervisor: ClaudeSupervisor,
}
impl Setup {
    fn new(line_delay_ms: u32) -> Self {
        let root = std::env::temp_dir().join(format!("formation-running-{}", RunId::generate()));
        fs::create_dir_all(&root).unwrap();
        let agent = Agent {
            id: AgentId::generate(),
            slug: "running".into(),
            name: "Running".into(),
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
                (
                    "FAKE_CLAUDE_LINE_DELAY_MS".into(),
                    line_delay_ms.to_string(),
                ),
            ]),
            ..ClaudeConfig::default()
        };
        let supervisor = ClaudeSupervisor::with_request_queue(
            config,
            store.clone(),
            sink.clone(),
            Some(queue.clone()),
        );
        Self {
            root,
            agent,
            store,
            sink,
            queue,
            supervisor,
        }
    }
    async fn enqueue(&self, prompt: &str) -> (QueueItem, RequestId, MemberId) {
        let requester = MemberId::generate();
        let item = self
            .queue
            .enqueue_request(work_request(
                requester,
                MemberId::generate(),
                self.agent.id,
                prompt,
                prompt,
            ))
            .await
            .unwrap();
        let QueueSource::Request { request_id, .. } = item.source else {
            unreachable!()
        };
        (item, request_id, requester)
    }
    /// Running-request lists in this agent's `agent.update` events, in order.
    fn lists(&self) -> Vec<Vec<RunningRequest>> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|(_, event)| match event {
                Event::AgentUpdate(AgentUpdateEvent::Upsert { agent })
                    if agent.id == self.agent.id =>
                {
                    Some(agent.running_requests)
                }
                _ => None,
            })
            .collect()
    }
    async fn progress(&self, count: usize) -> Vec<(MessageId, RunProgress)> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let progress = self.queue.progress();
                if progress.len() >= count {
                    break progress;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn stop_request(&self, request_id: RequestId) -> Result<serde_json::Value> {
        self.supervisor
            .handle(CommandCall {
                caller: Caller::Owner,
                method: "agent_request_stop".into(),
                params: json!({"agent_id": self.agent.id, "request_id": request_id}),
            })
            .await
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn running_request_is_listed_on_the_agent_while_its_fork_runs() {
    let s = Setup::new(10);
    s.supervisor.start(&s.agent, None).await.unwrap();
    let (item, request_id, requester) = s.enqueue("review the diff").await;
    let progress = s.progress(2).await;
    assert_eq!(progress[0], (item.id, RunProgress::Started));
    assert!(matches!(progress[1].1, RunProgress::Completed { .. }));

    let lists = s.lists();
    let listed = &lists
        .first()
        .expect("an agent.update when the fork started")[0];
    assert_eq!(listed.request_id, request_id);
    assert_eq!(listed.requester, requester);
    assert_eq!(listed.prompt, "review the diff");
    assert_eq!(listed.fork_session_id, None);
    assert!(
        lists.iter().flatten().any(|r| r.fork_session_id.is_some()),
        "the fork's session is filled in once known"
    );
    let fork = s.store.fork_session(listed.run_id).unwrap().unwrap();
    assert!(lists.iter().flatten().all(|r| {
        r.fork_session_id
            .as_ref()
            .is_none_or(|sid| *sid == fork.session_id)
    }));
    assert_eq!(lists.last().unwrap(), &vec![]);
    assert!(
        s.store
            .agent(s.agent.id)
            .unwrap()
            .unwrap()
            .running_requests
            .is_empty()
    );
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn agent_request_stop_interrupts_the_fork_and_reports_interrupted() {
    // Slow enough that the fork is still running when it is stopped.
    let s = Setup::new(300);
    s.supervisor.start(&s.agent, None).await.unwrap();
    assert_eq!(
        s.stop_request(RequestId::generate())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let (item, request_id, _) = s.enqueue("long job").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !s
            .lists()
            .iter()
            .flatten()
            .any(|r| r.request_id == request_id)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    s.stop_request(request_id).await.unwrap();
    assert_eq!(
        s.progress(2).await,
        vec![
            (item.id, RunProgress::Started),
            (item.id, RunProgress::Interrupted)
        ]
    );
    assert_eq!(s.lists().last().unwrap(), &vec![]);
    assert_eq!(
        s.stop_request(request_id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    s.supervisor.stop(s.agent.id, StopMode::Now).await.unwrap();
}

#[tokio::test]
async fn supervisor_start_clears_lists_left_by_a_previous_run() {
    let s = Setup::new(0);
    let mut stale = s.agent.clone();
    stale.running_requests = vec![RunningRequest {
        request_id: RequestId::generate(),
        requester: MemberId::generate(),
        prompt: "left over".into(),
        started_at: SystemTime::now().into(),
        run_id: RunId::generate(),
        fork_session_id: None,
    }];
    s.store.update_agent(&stale).unwrap();
    let _restarted = ClaudeSupervisor::new(
        ClaudeConfig::default(),
        s.store.clone(),
        Arc::new(RecordingEventSink::default()),
    );
    assert!(
        s.store
            .agent(s.agent.id)
            .unwrap()
            .unwrap()
            .running_requests
            .is_empty()
    );
}
