#![cfg(feature = "testkit")]

use formation_core::{ports::*, testkit::*};
use formation_proto::{
    agent::*,
    common::*,
    events::Event,
    harness::*,
    request::{QueueItem, QueueSource},
    snapshot::{Language, Settings, SettingsUpdateEvent},
};
use std::sync::Arc;

fn time(second: u32) -> Timestamp {
    format!("2026-11-09T00:00:{second:02}Z").parse().unwrap()
}
fn agent(slug: &str) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: slug.into(),
        name: slug.into(),
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
        created_at: time(0),
        last_activity_at: None,
        detected: None,
        running_requests: vec![],
    }
}
fn session(agent_id: AgentId, id: &str) -> SessionInfo {
    SessionInfo {
        agent_id,
        session_id: SessionId(id.into()),
        transcript_path: format!("/tmp/{id}.jsonl").into(),
        cwd: "/tmp/agent".into(),
        started_at: time(0),
        last_activity_at: None,
    }
}
fn item(agent_id: AgentId) -> QueueItem {
    QueueItem {
        id: MessageId::generate(),
        agent_id,
        source: QueueSource::Owner,
        text: "hello".into(),
        queued_at: time(0),
    }
}

fn harness_status() -> HarnessStatus {
    HarnessStatus {
        harness: HarnessKind::ClaudeCode,
        installed: true,
        signed_in: Some(true),
        version: Some("fake".into()),
        path: None,
        problem: None,
    }
}

#[test]
fn agents_enforce_slug_uniqueness_and_hide_soft_deleted_records() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let mut a = agent("one");
    store.insert_agent(&a).unwrap();
    assert_eq!(
        store.insert_agent(&agent("one")).unwrap_err().code,
        ErrorCode::Conflict
    );
    a.name = "updated".into();
    store.update_agent(&a).unwrap();
    assert_eq!(store.agent_by_slug("one").unwrap(), Some(a.clone()));
    let b = agent("two");
    store.insert_agent(&b).unwrap();
    a.slug = "two".into();
    assert_eq!(
        store.update_agent(&a).unwrap_err().code,
        ErrorCode::Conflict
    );
    store.delete_agent(a.id, time(1)).unwrap();
    assert_eq!(store.agents().unwrap(), vec![b]);
    assert!(store.agent(a.id).unwrap().is_none());
    assert!(store.agent_by_slug("one").unwrap().is_none());
    assert_eq!(
        store.update_agent(&a).unwrap_err().code,
        ErrorCode::NotFound
    );
    store.insert_agent(&agent("one")).unwrap();
}

#[test]
fn sessions_replace_only_the_matching_agents_session() {
    let store = MemoryStore::default();
    let id = AgentId::generate();
    assert_eq!(store.main_session(id).unwrap(), None);
    let other = session(AgentId::generate(), "other");
    store.put_main_session(&other).unwrap();
    store.put_main_session(&session(id, "old")).unwrap();
    let new = session(id, "new");
    store.put_main_session(&new).unwrap();
    assert_eq!(store.main_session(id).unwrap(), Some(new));
    assert_eq!(store.main_session(other.agent_id).unwrap(), Some(other));
}

#[test]
fn schedules_filter_upsert_delete_and_page_runs_newest_first() {
    let store = MemoryStore::default();
    let id = AgentId::generate();
    let mut schedule = Schedule {
        id: ScheduleId::generate(),
        agent_id: id,
        prompt: "run".into(),
        rule: ScheduleRule::Daily,
        time: "09:00:00".parse().unwrap(),
        paused: false,
        next_run_at: None,
        last_run: None,
    };
    store.put_schedule(&schedule).unwrap();
    schedule.paused = true;
    store.put_schedule(&schedule).unwrap();
    let other = Schedule {
        id: ScheduleId::generate(),
        agent_id: AgentId::generate(),
        ..schedule.clone()
    };
    store.put_schedule(&other).unwrap();
    assert_eq!(store.schedules(None).unwrap().len(), 2);
    assert_eq!(store.schedules(Some(id)).unwrap(), vec![schedule.clone()]);
    assert_eq!(store.schedule(schedule.id).unwrap(), Some(schedule.clone()));
    let mut runs = vec![];
    for second in [1, 3, 2] {
        let run = ScheduleRun {
            id: ScheduleRunId::generate(),
            schedule_id: schedule.id,
            planned_at: time(second),
            started_at: None,
            finished_at: None,
            result: None,
            error: None,
        };
        store.put_schedule_run(&run).unwrap();
        runs.push(run);
    }
    store
        .put_schedule_run(&ScheduleRun {
            id: ScheduleRunId::generate(),
            schedule_id: other.id,
            planned_at: time(4),
            ..runs[0].clone()
        })
        .unwrap();
    runs[2].result = Some(ScheduleRunResult::Completed);
    store.put_schedule_run(&runs[2]).unwrap();
    assert_eq!(
        store.schedule_runs(schedule.id, None, 10).unwrap(),
        vec![runs[1].clone(), runs[2].clone(), runs[0].clone()]
    );
    assert_eq!(
        store.schedule_runs(schedule.id, Some(time(3)), 1).unwrap(),
        vec![runs[2].clone()]
    );
    assert!(
        store
            .schedule_runs(schedule.id, None, 0)
            .unwrap()
            .is_empty()
    );
    store.delete_schedule(schedule.id).unwrap();
    assert_eq!(store.schedule(schedule.id).unwrap(), None);
    assert_eq!(store.schedules(None).unwrap(), vec![other]);
}

#[test]
fn attention_orders_open_items_and_resolves_every_matching_target() {
    let store = MemoryStore::default();
    let target = AttentionTarget::Agent {
        agent_id: AgentId::generate(),
    };
    let mut items = vec![];
    for second in [3, 1, 2] {
        let a = Attention {
            id: AttentionId::generate(),
            kind: AttentionKind::AgentError,
            target,
            subject: "error".into(),
            at: time(second),
            resolved_at: None,
        };
        store.put_attention(&a).unwrap();
        items.push(a);
    }
    items[2].target = AttentionTarget::Agent {
        agent_id: AgentId::generate(),
    };
    store.put_attention(&items[2]).unwrap();
    assert_eq!(
        store.open_attention().unwrap(),
        vec![items[1].clone(), items[2].clone(), items[0].clone()]
    );
    let resolved = store.resolve_attention(&target, time(5)).unwrap();
    assert_eq!(
        resolved.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![items[1].id, items[0].id]
    );
    assert!(resolved.iter().all(|a| a.resolved_at == Some(time(5))));
    assert_eq!(store.open_attention().unwrap(), vec![items[2].clone()]);
    assert!(
        store
            .resolve_attention(&target, time(6))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn activity_filters_before_limit_and_orders_newest_first() {
    let store = MemoryStore::default();
    let id = AgentId::generate();
    let member = MemberId::generate();
    let mut activities = vec![];
    for second in [2, 4, 1, 3] {
        let a = Activity {
            id: ActivityId::generate(),
            kind: ActivityKind::AgentStarted,
            actor: Some(member),
            agent_id: Some(id),
            target: None,
            at: time(second),
            summary: "started".into(),
        };
        store.record_activity(&a).unwrap();
        activities.push(a);
    }
    store
        .record_activity(&Activity {
            id: ActivityId::generate(),
            actor: None,
            at: time(6),
            ..activities[0].clone()
        })
        .unwrap();
    store
        .record_activity(&Activity {
            id: ActivityId::generate(),
            agent_id: Some(AgentId::generate()),
            at: time(5),
            ..activities[0].clone()
        })
        .unwrap();
    assert_eq!(
        store.activity(Some(id), Some(member), None, 2).unwrap(),
        vec![activities[1].clone(), activities[3].clone()]
    );
    assert_eq!(
        store
            .activity(Some(id), Some(member), Some(time(3)), 10)
            .unwrap(),
        vec![activities[0].clone(), activities[2].clone()]
    );
    assert_eq!(store.activity(None, None, None, 10).unwrap().len(), 6);
    assert!(store.activity(None, None, None, 0).unwrap().is_empty());
    store
        .delete_activity(&[activities[0].id, activities[2].id])
        .unwrap();
    assert!(
        store
            .activity(None, None, Some(time(3)), 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn settings_default_and_replace() {
    let store = MemoryStore::default();
    assert_eq!(store.settings().unwrap(), Settings::default());
    let settings = Settings {
        language: Some(Language::Ko),
        ..Default::default()
    };
    store.put_settings(&settings).unwrap();
    assert_eq!(store.settings().unwrap(), settings);
}

#[test]
fn recording_sink_numbers_and_retains_events_in_order() {
    let sink = RecordingEventSink::default();
    let events: Vec<_> = [None, Some(Language::Ko), Some(Language::En)]
        .into_iter()
        .map(|language| {
            Event::SettingsUpdate(SettingsUpdateEvent {
                settings: Settings {
                    language,
                    ..Default::default()
                },
            })
        })
        .collect();
    for (i, event) in events.iter().enumerate() {
        assert_eq!(sink.publish(event.clone()), Seq(i as u64 + 1));
    }
    assert_eq!(
        sink.events(),
        events
            .into_iter()
            .enumerate()
            .map(|(i, event)| (Seq(i as u64 + 1), event))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn harness_returns_scripts_records_calls_and_allows_explicit_stream_events() {
    let a = agent("one");
    let session = session(a.id, "scripted");
    let turns = vec![
        TurnResult {
            outcome: TurnOutcome::Completed,
            result: Some("first".into()),
        },
        TurnResult {
            outcome: TurnOutcome::Interrupted,
            result: None,
        },
    ];
    let mut script = ScriptedAgent::new(session.clone(), turns.clone());
    let page = ConversationPageResult {
        items: vec![],
        has_more: false,
        stream_seq: 7,
    };
    script.pages.push(ScriptedPage {
        session_id: None,
        before: Some(ItemId("cursor".into())),
        limit: 3,
        page: page.clone(),
    });
    let fork_page = ConversationPageResult {
        stream_seq: 8,
        ..page.clone()
    };
    script.pages.push(ScriptedPage {
        session_id: Some(SessionId("fork".into())),
        before: Some(ItemId("cursor".into())),
        limit: 3,
        page: fork_page.clone(),
    });
    let status = harness_status();
    let fake = Arc::new(FakeHarness::new(
        HarnessKind::ClaudeCode,
        status.clone(),
        [script],
    ));
    let harness: Arc<dyn Harness> = fake.clone();
    assert_eq!(harness.kind(), HarnessKind::ClaudeCode);
    assert_eq!(harness.status().await, status);
    assert_eq!(harness.start(&a, None).await.unwrap(), session);
    assert_eq!(
        harness.session_info(a.id).await.unwrap(),
        Some(session.clone())
    );
    let item = item(a.id);
    let mut receiver = harness.subscribe();
    let run_id = RunId::generate();
    for turn in turns {
        assert_eq!(
            harness.run_turn(a.id, run_id, item.clone()).await.unwrap(),
            turn
        );
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        harness
            .run_turn(a.id, run_id, item.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness
            .conversation(a.id, None, Some(ItemId("cursor".into())), 3)
            .await
            .unwrap(),
        page
    );
    assert_eq!(
        harness
            .conversation(
                a.id,
                Some(session.session_id.clone()),
                Some(ItemId("cursor".into())),
                3
            )
            .await
            .unwrap(),
        page
    );
    assert_eq!(
        harness
            .conversation(
                a.id,
                Some(SessionId("fork".into())),
                Some(ItemId("cursor".into())),
                3
            )
            .await
            .unwrap(),
        fork_page
    );
    assert_eq!(
        harness
            .conversation(
                a.id,
                Some(SessionId("missing".into())),
                Some(ItemId("cursor".into())),
                3
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    harness.steer(a.id, item.clone()).await.unwrap();
    harness.interrupt(a.id).await.unwrap();
    harness.stop(a.id, StopMode::AfterTurn).await.unwrap();
    assert_eq!(
        fake.calls(),
        vec![
            HarnessCall::RunTurn {
                agent_id: a.id,
                run_id,
                item: item.clone()
            },
            HarnessCall::RunTurn {
                agent_id: a.id,
                run_id,
                item: item.clone()
            },
            HarnessCall::RunTurn {
                agent_id: a.id,
                run_id,
                item: item.clone()
            },
            HarnessCall::Steer {
                agent_id: a.id,
                item: item.clone()
            },
            HarnessCall::Interrupt { agent_id: a.id },
            HarnessCall::Stop {
                agent_id: a.id,
                mode: StopMode::AfterTurn
            }
        ]
    );
    let event = AgentStreamEvent {
        agent_id: a.id,
        session_id: session.session_id,
        stream_seq: 1,
        delta: StreamDelta::TurnStarted {
            message_id: item.id,
        },
    };
    fake.sender().send(event.clone()).unwrap();
    assert_eq!(receiver.recv().await.unwrap(), event);
    let missing = agent("missing");
    assert_eq!(
        harness.start(&missing, None).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness.session_info(missing.id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness
            .run_turn(missing.id, run_id, item.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness.steer(missing.id, item).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness.interrupt(missing.id).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness
            .stop(missing.id, StopMode::Now)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness
            .conversation(missing.id, None, None, 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn harness_run_returns_only_the_scripted_run_info() {
    let fake = FakeHarness::new(HarnessKind::ClaudeCode, harness_status(), []);
    let agent_id = AgentId::generate();
    let run_id = RunId::generate();
    assert!(fake.run(run_id).await.is_none());
    let info = RunInfo {
        run_id,
        agent_id,
        session_id: SessionId("fork".into()),
        item: item(agent_id),
    };
    fake.set_run(info.clone());
    let stored = fake.run(run_id).await.unwrap();
    assert_eq!(stored.run_id, info.run_id);
    assert_eq!(stored.agent_id, info.agent_id);
    assert_eq!(stored.session_id, info.session_id);
    assert_eq!(stored.item, info.item);
    assert!(fake.run(RunId::generate()).await.is_none());
    let second = RunInfo {
        run_id: RunId::generate(),
        ..info.clone()
    };
    fake.set_run(second.clone());
    assert_eq!(fake.run(run_id).await.unwrap().run_id, run_id);
    assert_eq!(fake.run(second.run_id).await.unwrap().run_id, second.run_id);
}

#[tokio::test]
async fn harness_forks_consume_only_the_agents_script_and_record_the_request() {
    let agent_id = AgentId::generate();
    let main = session(agent_id, "main");
    let results: Vec<_> = ["fork-one", "fork-two"]
        .into_iter()
        .map(|id| ForkResult {
            session: session(agent_id, id),
            result: TurnResult {
                outcome: TurnOutcome::Completed,
                result: Some(id.into()),
            },
        })
        .collect();
    let mut script = ScriptedAgent::new(main.clone(), []);
    script.forks = results.clone().into();
    let other = session(AgentId::generate(), "other");
    let fake = FakeHarness::new(
        HarnessKind::ClaudeCode,
        harness_status(),
        [script, ScriptedAgent::new(other.clone(), [])],
    );
    let fork = ForkRun {
        run_id: RunId::generate(),
        from: main.session_id.clone(),
        item: item(agent_id),
        tools: ForkTools::None,
    };
    assert_eq!(
        fake.run_fork(other.agent_id, fork.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    for result in results {
        assert_eq!(fake.run_fork(agent_id, fork.clone()).await.unwrap(), result);
    }
    assert_eq!(
        fake.run_fork(agent_id, fork.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        fake.run_fork(AgentId::generate(), fork.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let expected = |agent_id| HarnessCall::RunFork {
        agent_id,
        run_id: fork.run_id,
        from: fork.from.clone(),
        item: fork.item.clone(),
        tools: fork.tools,
    };
    assert_eq!(
        fake.calls(),
        vec![
            expected(other.agent_id),
            expected(agent_id),
            expected(agent_id),
            expected(agent_id)
        ]
    );
    assert_eq!(fake.session_info(agent_id).await.unwrap(), Some(main));
}

#[test]
fn fork_sessions_upsert_by_run_and_preserve_main_and_other_forks() {
    let store = MemoryStore::default();
    let agent_id = AgentId::generate();
    let run_id = RunId::generate();
    let other_run = RunId::generate();
    assert_eq!(store.fork_session(run_id).unwrap(), None);
    let main = session(agent_id, "main");
    store.put_main_session(&main).unwrap();
    let other = session(agent_id, "other-fork");
    store.put_fork_session(other_run, &other).unwrap();
    store
        .put_fork_session(run_id, &session(agent_id, "old-fork"))
        .unwrap();
    let new = session(agent_id, "new-fork");
    store.put_fork_session(run_id, &new).unwrap();
    assert_eq!(store.fork_session(run_id).unwrap(), Some(new.clone()));
    assert_eq!(store.fork_session(other_run).unwrap(), Some(other));
    assert_eq!(store.main_session(agent_id).unwrap(), Some(main));
    store
        .put_main_session(&session(agent_id, "new-main"))
        .unwrap();
    assert_eq!(store.fork_session(run_id).unwrap(), Some(new));
}
