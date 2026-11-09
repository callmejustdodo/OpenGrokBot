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
        before: Some(ItemId("cursor".into())),
        limit: 3,
        page: page.clone(),
    });
    let status = HarnessStatus {
        harness: HarnessKind::ClaudeCode,
        installed: true,
        signed_in: Some(true),
        version: Some("fake".into()),
        path: None,
        problem: None,
    };
    let fake = Arc::new(FakeHarness::new(
        HarnessKind::ClaudeCode,
        status.clone(),
        [script],
    ));
    let harness: Arc<dyn Harness> = fake.clone();
    assert_eq!(harness.kind(), HarnessKind::ClaudeCode);
    assert_eq!(harness.status().await, status);
    assert_eq!(harness.start(&a, None).await.unwrap(), session);
    assert_eq!(harness.session_info(a.id).await.unwrap(), Some(session));
    assert_eq!(harness.current_turn(a.id).await, None);
    let item = item(a.id);
    fake.set_current_turn(a.id, Some(item.clone())).unwrap();
    assert_eq!(harness.current_turn(a.id).await, Some(item.clone()));
    let mut receiver = harness.subscribe();
    for turn in turns {
        assert_eq!(harness.run_turn(a.id, item.clone()).await.unwrap(), turn);
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(
        harness.run_turn(a.id, item.clone()).await.unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        harness
            .conversation(a.id, Some(ItemId("cursor".into())), 3)
            .await
            .unwrap(),
        page
    );
    harness.steer(a.id, item.clone()).await.unwrap();
    harness.interrupt(a.id).await.unwrap();
    harness.stop(a.id, StopMode::AfterTurn).await.unwrap();
    assert_eq!(
        fake.calls(),
        vec![
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
            .run_turn(missing.id, item.clone())
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
            .conversation(missing.id, None, 1)
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}
