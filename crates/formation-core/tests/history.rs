#![cfg(feature = "testkit")]
//! `history_list` and `history_delete`, and the activity A records itself.

use std::{collections::HashMap, sync::Arc, time::Duration as StdDuration};

use chrono::{Duration, Utc};
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    history::History,
    ports::*,
    schedules::Schedules,
    testkit::*,
};
use formation_proto::{
    agent::*,
    common::*,
    harness::*,
    policy::{Caller, Chain},
};
use serde_json::{Value, json};

fn agent(state: AgentState, folder: std::path::PathBuf) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "support".into(),
        name: "Support".into(),
        role: "support".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: folder,
        repositories: vec![],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state,
        files_revision: "1".into(),
        created_at: Utc::now(),
        last_activity_at: None,
        detected: None,
        running_requests: vec![],
    }
}

fn activity(kind: ActivityKind, actor: Option<MemberId>, agent_id: AgentId, at: i64) -> Activity {
    Activity {
        id: ActivityId::generate(),
        kind,
        actor,
        agent_id: Some(agent_id),
        target: None,
        at: Utc::now() - Duration::minutes(at),
        summary: format!("{at} minutes ago"),
    }
}

async fn call(history: &History, method: &str, params: Value) -> Result<Value> {
    history
        .handle(CommandCall {
            caller: Caller::Owner,
            method: method.into(),
            params,
        })
        .await
}

fn list(
    agent_id: Option<AgentId>,
    member_id: Option<MemberId>,
    before: Option<Timestamp>,
    limit: u32,
) -> Value {
    json!({"agent_id": agent_id, "member_id": member_id, "before": before, "limit": limit})
}

#[tokio::test]
async fn history_list_filters_and_limits_then_history_delete_removes() {
    let store = Arc::new(MemoryStore::default());
    let history = History::new(store.clone());
    let (one, two) = (AgentId::generate(), AgentId::generate());
    let member = MemberId::generate();
    let newest = activity(ActivityKind::AgentStarted, None, one, 1);
    let middle = activity(ActivityKind::RequestReceived, Some(member), two, 2);
    let oldest = activity(ActivityKind::ScheduleRan, None, one, 3);
    for item in [&oldest, &newest, &middle] {
        store.record_activity(item).unwrap();
    }
    let read = |value: Value| serde_json::from_value::<Vec<Activity>>(value).unwrap();

    let all = read(
        call(&history, "history_list", list(None, None, None, 10))
            .await
            .unwrap(),
    );
    assert_eq!(all, vec![newest.clone(), middle.clone(), oldest.clone()]);
    let limited = read(
        call(&history, "history_list", list(None, None, None, 2))
            .await
            .unwrap(),
    );
    assert_eq!(limited, vec![newest.clone(), middle.clone()]);
    // The filter applies before the limit.
    let by_agent = read(
        call(&history, "history_list", list(Some(one), None, None, 1))
            .await
            .unwrap(),
    );
    assert_eq!(by_agent, vec![newest.clone()]);
    let by_member = read(
        call(&history, "history_list", list(None, Some(member), None, 10))
            .await
            .unwrap(),
    );
    assert_eq!(by_member, vec![middle.clone()]);
    let before = read(
        call(
            &history,
            "history_list",
            list(None, None, Some(middle.at), 10),
        )
        .await
        .unwrap(),
    );
    assert_eq!(before, vec![oldest.clone()]);

    let deleted = call(
        &history,
        "history_delete",
        json!({"ids": [newest.id, oldest.id]}),
    )
    .await
    .unwrap();
    assert_eq!(deleted, Value::Null);
    let left = read(
        call(&history, "history_list", list(None, None, None, 10))
            .await
            .unwrap(),
    );
    assert_eq!(left, vec![middle]);

    let bad = call(&history, "history_list", json!({})).await.unwrap_err();
    assert_eq!(bad.code, ErrorCode::InvalidParams);
    let denied = history
        .handle(CommandCall {
            caller: Caller::Member {
                member_id: member,
                via_agent: None,
                chain: Chain {
                    id: ChainId::generate(),
                    depth: 0,
                },
            },
            method: "history_list".into(),
            params: list(None, None, None, 10),
        })
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::Denied);
}

#[derive(Default)]
struct NoNotifier;

#[async_trait::async_trait]
impl Notifier for NoNotifier {
    async fn raise(&self, _: AttentionKind, _: AttentionTarget, _: String) -> Result<Attention> {
        unreachable!("a completed run raises nothing")
    }
    async fn resolve(&self, _: AttentionTarget) -> Result<()> {
        Ok(())
    }
    async fn inbox(&self) -> Result<Vec<Attention>> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn scheduled_run_records_schedule_ran() {
    let agent = agent(AgentState::Idle, "/tmp/support".into());
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let session = SessionInfo {
        agent_id: agent.id,
        session_id: SessionId("main".into()),
        transcript_path: "/tmp/main.jsonl".into(),
        cwd: agent.folder_path.clone(),
        started_at: Utc::now(),
        last_activity_at: None,
    };
    let harness = Arc::new(FakeHarness::new(
        HarnessKind::ClaudeCode,
        HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: true,
            signed_in: Some(true),
            version: None,
            path: None,
            problem: None,
        },
        [ScriptedAgent {
            forks: vec![ForkResult {
                session: session.clone(),
                result: TurnResult {
                    outcome: TurnOutcome::Completed,
                    result: None,
                },
            }]
            .into(),
            ..ScriptedAgent::new(session, [])
        }],
    ));
    let schedules = Schedules::new(
        store.clone(),
        harness,
        Arc::new(RecordingEventSink::default()),
        Arc::new(NoNotifier),
    );
    let schedule: Schedule = serde_json::from_value(
        schedules
            .handle(CommandCall {
                caller: Caller::Owner,
                method: "schedule_create".into(),
                params: json!({
                    "agent_id": agent.id, "prompt": "summarize tickets",
                    "rule": ScheduleRule::Daily, "time": "09:00:00"
                }),
            })
            .await
            .unwrap(),
    )
    .unwrap();
    let due = schedule.next_run_at.unwrap();
    schedules.tick(due).await.unwrap();

    let run = store
        .schedule(schedule.id)
        .unwrap()
        .unwrap()
        .last_run
        .unwrap();
    let history = History::new(store.clone());
    let listed: Vec<Activity> = serde_json::from_value(
        call(
            &history,
            "history_list",
            list(Some(agent.id), None, None, 10),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(listed.len(), 1);
    let ran = &listed[0];
    assert_eq!(ran.kind, ActivityKind::ScheduleRan);
    assert_eq!(ran.actor, None);
    assert_eq!(ran.agent_id, Some(agent.id));
    assert_eq!(
        ran.target,
        Some(AttentionTarget::Schedule {
            schedule_id: schedule.id
        })
    );
    assert_eq!(Some(ran.at), run.finished_at);
    assert_eq!(ran.summary, "summarize tickets");
}

#[tokio::test]
async fn claude_start_and_stop_record_lifecycle() {
    let root = std::env::temp_dir().join(format!("formation-history-{}", RunId::generate()));
    std::fs::create_dir_all(&root).unwrap();
    let agent = agent(AgentState::Stopped, root.clone());
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let supervisor = ClaudeSupervisor::new(
        ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            stalled_after: StdDuration::from_secs(600),
            env: HashMap::from([(
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            )]),
            ..ClaudeConfig::default()
        },
        store.clone(),
        Arc::new(RecordingEventSink::default()),
    );
    supervisor.start(&agent, None).await.unwrap();
    supervisor.stop(agent.id, StopMode::Now).await.unwrap();

    let recorded: Vec<(ActivityKind, String)> = store
        .activity(Some(agent.id), None, None, 10)
        .unwrap()
        .into_iter()
        .map(|a| (a.kind, a.summary))
        .collect();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    for kind in [ActivityKind::AgentStarted, ActivityKind::AgentStopped] {
        assert!(
            recorded.contains(&(kind, "Support".to_string())),
            "{recorded:?}"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn claude_process_exit_records_agent_failed() {
    let root = std::env::temp_dir().join(format!("formation-history-{}", RunId::generate()));
    std::fs::create_dir_all(&root).unwrap();
    let agent = agent(AgentState::Stopped, root.clone());
    let store = Arc::new(MemoryStore::default());
    store.insert_agent(&agent).unwrap();
    let supervisor = ClaudeSupervisor::new(
        ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            stalled_after: StdDuration::from_secs(600),
            // fake-claude exits at once when it cannot read its transcript.
            env: HashMap::from([(
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                root.join("missing.jsonl").display().to_string(),
            )]),
            ..ClaudeConfig::default()
        },
        store.clone(),
        Arc::new(RecordingEventSink::default()),
    );
    supervisor.start(&agent, None).await.unwrap();

    let failed = tokio::time::timeout(StdDuration::from_secs(5), async {
        loop {
            if let Some(failed) = store
                .activity(Some(agent.id), None, None, 10)
                .unwrap()
                .into_iter()
                .find(|a| a.kind == ActivityKind::AgentFailed)
            {
                break failed;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("agent_failed recorded");
    assert_eq!(failed.actor, None);
    assert_eq!(failed.agent_id, Some(agent.id));
    assert_eq!(failed.target, None);
    assert!(
        failed.summary.starts_with("Claude exited"),
        "{}",
        failed.summary
    );
    let kinds: Vec<ActivityKind> = store
        .activity(Some(agent.id), None, None, 10)
        .unwrap()
        .into_iter()
        .map(|a| a.kind)
        .collect();
    assert!(!kinds.contains(&ActivityKind::AgentStopped), "{kinds:?}");
    std::fs::remove_dir_all(root).unwrap();
}
