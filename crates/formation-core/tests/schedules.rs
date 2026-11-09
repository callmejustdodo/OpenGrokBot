#![cfg(feature = "testkit")]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Datelike, Duration, Local, TimeZone, Utc};
use formation_core::{
    harness::claude::{ClaudeConfig, ClaudeSupervisor},
    ports::*,
    schedules::Schedules,
    testkit::*,
};
use formation_proto::{
    agent::*, common::*, events::Event, harness::*, policy::Caller, request::QueueSource,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Notifications(Mutex<Vec<Attention>>);
#[async_trait]
impl Notifier for Notifications {
    async fn raise(
        &self,
        kind: AttentionKind,
        target: AttentionTarget,
        subject: String,
    ) -> Result<Attention> {
        let attention = Attention {
            id: AttentionId::generate(),
            kind,
            target,
            subject,
            at: Utc::now(),
            resolved_at: None,
        };
        self.0.lock().unwrap().push(attention.clone());
        Ok(attention)
    }
    async fn resolve(&self, _: AttentionTarget) -> Result<()> {
        Ok(())
    }
    async fn inbox(&self) -> Result<Vec<Attention>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

fn agent() -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "worker".into(),
        name: "Worker".into(),
        role: "developer".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: "/tmp/agent".into(),
        repositories: vec![],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state: AgentState::Idle,
        files_revision: "1".into(),
        created_at: Utc::now(),
        last_activity_at: None,
        detected: None,
    }
}
struct Fixture {
    schedules: Schedules,
    store: Arc<MemoryStore>,
    harness: Arc<FakeHarness>,
    events: Arc<RecordingEventSink>,
    notifier: Arc<Notifications>,
    agent: Agent,
}
impl Fixture {
    fn new(outcomes: &[TurnOutcome]) -> Self {
        let agent = agent();
        let store = Arc::new(MemoryStore::default());
        store.insert_agent(&agent).unwrap();
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
                forks: outcomes
                    .iter()
                    .map(|outcome| ForkResult {
                        session: SessionInfo {
                            agent_id: agent.id,
                            session_id: SessionId(RunId::generate().to_string()),
                            transcript_path: "/tmp/fork.jsonl".into(),
                            cwd: agent.folder_path.clone(),
                            started_at: Utc::now(),
                            last_activity_at: None,
                        },
                        result: TurnResult {
                            outcome: *outcome,
                            result: None,
                        },
                    })
                    .collect(),
                ..ScriptedAgent::new(
                    SessionInfo {
                        agent_id: agent.id,
                        session_id: SessionId("main".into()),
                        transcript_path: "/tmp/main.jsonl".into(),
                        cwd: agent.folder_path.clone(),
                        started_at: Utc::now(),
                        last_activity_at: None,
                    },
                    [],
                )
            }],
        ));
        let events = Arc::new(RecordingEventSink::default());
        let notifier = Arc::new(Notifications::default());
        let schedules = Schedules::new(
            store.clone(),
            harness.clone(),
            events.clone(),
            notifier.clone(),
        );
        Self {
            schedules,
            store,
            harness,
            events,
            notifier,
            agent,
        }
    }
    async fn call(&self, method: &str, params: Value) -> Value {
        self.schedules
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
            .unwrap()
    }
    async fn create(&self, rule: ScheduleRule) -> Schedule {
        serde_json::from_value(self.call("schedule_create", json!({
            "agent_id": self.agent.id, "prompt": "summarize tickets", "rule": rule, "time": "09:00:00"
        })).await).unwrap()
    }
}

#[tokio::test]
async fn commands_persist_publish_pause_resume_and_delete() {
    let f = Fixture::new(&[]);
    let schedule = f.create(ScheduleRule::Daily).await;
    assert!(schedule.next_run_at.unwrap() > Utc::now());
    let listed: Vec<Schedule> = serde_json::from_value(
        f.call("schedule_list", json!({"agent_id": f.agent.id}))
            .await,
    )
    .unwrap();
    assert_eq!(listed, vec![schedule.clone()]);
    f.call(
        "schedule_update",
        json!({"schedule_id": schedule.id, "paused": true, "prompt": "updated"}),
    )
    .await;
    let paused = f.store.schedule(schedule.id).unwrap().unwrap();
    assert!(paused.paused);
    assert_eq!(paused.next_run_at, None);
    f.schedules
        .tick(Utc::now() + Duration::days(10))
        .await
        .unwrap();
    assert!(f.harness.calls().is_empty());
    f.call(
        "schedule_update",
        json!({"schedule_id": schedule.id, "paused": false}),
    )
    .await;
    let resumed = f.store.schedule(schedule.id).unwrap().unwrap();
    assert!(!resumed.paused);
    assert_eq!(resumed.prompt, "updated");
    assert!(resumed.next_run_at.is_some());
    f.call("schedule_delete", json!({"schedule_id": schedule.id}))
        .await;
    assert!(f.store.schedules(None).unwrap().is_empty());
    let events = f.events.events();
    assert_eq!(events.len(), 4);
    assert!(matches!(
        events[0].1,
        Event::ScheduleUpdate(ScheduleUpdateEvent::Upsert { .. })
    ));
    assert!(
        matches!(events[3].1, Event::ScheduleUpdate(ScheduleUpdateEvent::Removed { schedule_id }) if schedule_id == schedule.id)
    );
}

#[tokio::test]
async fn due_fork_preserves_main_session_records_completion_and_skips_future_work() {
    let f = Fixture::new(&[TurnOutcome::Completed]);
    let main = f.harness.session_info(f.agent.id).await.unwrap().unwrap();
    let schedule = f.create(ScheduleRule::Daily).await;
    let due = schedule.next_run_at.unwrap();
    f.schedules.tick(due - Duration::seconds(1)).await.unwrap();
    assert!(f.harness.calls().is_empty());
    f.schedules.tick(due).await.unwrap();
    let updated = f.store.schedule(schedule.id).unwrap().unwrap();
    assert!(updated.next_run_at.unwrap() > due);
    let run = updated.last_run.unwrap();
    assert_eq!(run.result, Some(ScheduleRunResult::Completed));
    assert_eq!(run.planned_at, due);
    assert_eq!(run.started_at, Some(due));
    assert!(run.finished_at.unwrap() >= due);
    let calls = f.harness.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        matches!(&calls[0], HarnessCall::RunFork { agent_id, from, item, tools, .. }
        if *agent_id == f.agent.id && *from == main.session_id && *tools == ForkTools::Gated
        && item.text == schedule.prompt && item.queued_at == due
        && item.source == QueueSource::Schedule { schedule_id: schedule.id })
    );
    assert_eq!(
        f.harness.session_info(f.agent.id).await.unwrap(),
        Some(main)
    );
    let history: Vec<ScheduleRun> = serde_json::from_value(
        f.call(
            "schedule_run_list",
            json!({"schedule_id": schedule.id, "limit": 10}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(history, vec![run]);
    assert_eq!(f.events.events().len(), 3);
    assert!(f.notifier.inbox().await.unwrap().is_empty());
}

#[tokio::test]
async fn due_run_without_main_session_falls_back_to_turn() {
    let mut f = Fixture::new(&[]);
    let root = std::env::temp_dir().join(format!("formation-schedule-{}", RunId::generate()));
    std::fs::create_dir_all(&root).unwrap();
    f.agent.folder_path = root.clone();
    f.store.update_agent(&f.agent).unwrap();
    let harness = Arc::new(ClaudeSupervisor::new(
        ClaudeConfig {
            binary: env!("CARGO_BIN_EXE_fake-claude").into(),
            config_dir: root.join("config"),
            env: std::collections::HashMap::from([(
                "FAKE_CLAUDE_TRANSCRIPT".into(),
                format!(
                    "{}/testkit/fixtures/claude/1-one-turn.stdout.jsonl",
                    env!("CARGO_MANIFEST_DIR")
                ),
            )]),
            ..ClaudeConfig::default()
        },
        f.store.clone(),
        f.events.clone(),
    ));
    f.schedules = Schedules::new(
        f.store.clone(),
        harness.clone(),
        f.events.clone(),
        f.notifier.clone(),
    );
    assert_eq!(harness.session_info(f.agent.id).await.unwrap(), None);
    let schedule = f.create(ScheduleRule::Daily).await;
    f.schedules
        .tick(schedule.next_run_at.unwrap())
        .await
        .unwrap();
    let run = f
        .store
        .schedule(schedule.id)
        .unwrap()
        .unwrap()
        .last_run
        .unwrap();
    assert_eq!(run.result, Some(ScheduleRunResult::Completed));
    assert!(harness.session_info(f.agent.id).await.unwrap().is_some());
    assert!(f.notifier.inbox().await.unwrap().is_empty());
    harness.stop(f.agent.id, StopMode::Now).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn wake_runs_once_for_many_missed_slots() {
    let f = Fixture::new(&[TurnOutcome::Completed]);
    let schedule = f.create(ScheduleRule::EveryHours { hours: 2 }).await;
    let planned = schedule.next_run_at.unwrap();
    let wake = planned + Duration::hours(11);
    f.schedules.tick(wake).await.unwrap();
    f.schedules.tick(wake).await.unwrap();
    assert_eq!(f.harness.calls().len(), 1);
    let updated = f.store.schedule(schedule.id).unwrap().unwrap();
    assert_eq!(
        updated.last_run.unwrap().result,
        Some(ScheduleRunResult::MissedRan)
    );
    assert_eq!(updated.next_run_at, Some(planned + Duration::hours(12)));
    assert_eq!(
        f.store.schedule_runs(schedule.id, None, 10).unwrap().len(),
        1
    );
}

#[tokio::test]
async fn failed_turn_and_harness_error_record_failure_and_alert() {
    let f = Fixture::new(&[TurnOutcome::Failed]);
    let schedule = f.create(ScheduleRule::Daily).await;
    f.schedules
        .tick(schedule.next_run_at.unwrap())
        .await
        .unwrap();
    let updated = f.store.schedule(schedule.id).unwrap().unwrap();
    f.schedules
        .tick(updated.next_run_at.unwrap())
        .await
        .unwrap(); // No scripted response: port error.
    let runs = f.store.schedule_runs(schedule.id, None, 10).unwrap();
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter()
            .all(|r| r.result == Some(ScheduleRunResult::Failed) && r.finished_at.is_some())
    );
    let alerts = f.notifier.inbox().await.unwrap();
    assert_eq!(alerts.len(), 2);
    assert!(
        alerts
            .iter()
            .all(|a| a.kind == AttentionKind::ScheduleFailed
                && a.target
                    == AttentionTarget::Schedule {
                        schedule_id: schedule.id
                    })
    );
}

#[tokio::test]
async fn calendar_rules_use_local_time_and_weekdays() {
    let f = Fixture::new(&[TurnOutcome::Completed, TurnOutcome::Completed]);
    for rule in [
        ScheduleRule::Daily,
        ScheduleRule::Weekdays,
        ScheduleRule::Weekly {
            weekday: Weekday::Mon,
        },
    ] {
        let schedule = f.create(rule).await;
        let local = schedule.next_run_at.unwrap().with_timezone(&Local);
        assert_eq!(local.time(), schedule.time);
        match rule {
            ScheduleRule::Weekdays => assert!(local.weekday().number_from_monday() <= 5),
            ScheduleRule::Weekly { .. } => assert_eq!(local.weekday(), chrono::Weekday::Mon),
            _ => (),
        }
        f.call("schedule_delete", json!({"schedule_id": schedule.id}))
            .await;
    }
    // A Friday slot missed over the weekend runs once on Monday, then moves to Tuesday.
    let mut schedule = f.create(ScheduleRule::Weekdays).await;
    let friday = Local
        .with_ymd_and_hms(2030, 1, 4, 9, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    schedule.next_run_at = Some(friday);
    f.store.put_schedule(&schedule).unwrap();
    let monday = Local
        .with_ymd_and_hms(2030, 1, 7, 10, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
    f.schedules.tick(monday).await.unwrap();
    let next = f
        .store
        .schedule(schedule.id)
        .unwrap()
        .unwrap()
        .next_run_at
        .unwrap()
        .with_timezone(&Local);
    assert_eq!(next.weekday(), chrono::Weekday::Tue);
    assert_eq!(next.time(), schedule.time);
}
