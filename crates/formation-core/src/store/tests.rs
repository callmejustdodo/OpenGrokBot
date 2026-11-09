use std::sync::Arc;

use formation_proto::{
    agent::*,
    common::*,
    harness::SessionInfo,
    snapshot::{Language, Settings},
};

use super::*;
use crate::ports::*;

fn store() -> SqliteStore {
    SqliteStore::in_memory(&[MIGRATIONS]).unwrap()
}
fn at(second: u32) -> Timestamp {
    format!("2026-11-09T00:00:{second:02}Z").parse().unwrap()
}
fn agent() -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: "builder".into(),
        name: "Builder".into(),
        role: "dev".into(),
        harness: HarnessKind::Detected,
        folder_path: "/tmp/한글".into(),
        repositories: vec!["/tmp/repo".into()],
        avatar: Avatar {
            shape: "bot".into(),
            color: "blue".into(),
        },
        state: AgentState::Working,
        files_revision: "rev-1".into(),
        created_at: at(0),
        last_activity_at: Some(at(1)),
        detected: Some(DetectedSource {
            harness: DetectedHarness::Codex,
            path: "/tmp/source".into(),
            capabilities: DetectedCapabilities {
                state: Capability {
                    supported: true,
                    reason: None,
                },
                read_conversation: Capability {
                    supported: true,
                    reason: None,
                },
                answer_teammates: Capability {
                    supported: false,
                    reason: Some("unsupported".into()),
                },
                receive_requests: Capability {
                    supported: false,
                    reason: None,
                },
            },
        }),
    }
}
fn session(agent_id: AgentId) -> SessionInfo {
    SessionInfo {
        agent_id,
        session_id: SessionId("opaque-thread".into()),
        transcript_path: "/tmp/transcript".into(),
        cwd: "/tmp/cwd".into(),
        started_at: at(0),
        last_activity_at: Some(at(1)),
    }
}
fn schedule(agent_id: AgentId) -> Schedule {
    Schedule {
        id: ScheduleId::generate(),
        agent_id,
        prompt: "Build".into(),
        rule: ScheduleRule::Weekly {
            weekday: Weekday::Tue,
        },
        time: "09:30:00".parse().unwrap(),
        paused: true,
        next_run_at: Some(at(20)),
        last_run: None,
    }
}

#[test]
fn fresh_schema_and_file_reopen() {
    let path = std::env::temp_dir().join(format!("formation-store-{}.db", AgentId::generate()));
    let a = agent();
    {
        let store = SqliteStore::open(&path, &[MIGRATIONS]).unwrap();
        store
            .with_connection(|c| {
                for name in [
                    "agents",
                    "main_sessions",
                    "fork_sessions",
                    "schedules",
                    "schedule_runs",
                    "attention",
                    "activity",
                    "settings",
                ] {
                    let count: i64 = c
                        .query_row(
                            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?",
                            [name],
                            |r| r.get(0),
                        )
                        .unwrap();
                    assert_eq!(count, 1, "{name}");
                }
                let fk: i64 = c
                    .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(fk, 1);
                Ok(())
            })
            .unwrap();
        assert_eq!(store.migrate(&[MIGRATIONS]).unwrap(), 0);
        store.insert_agent(&a).unwrap();
    }
    let reopened = SqliteStore::open(&path, &[MIGRATIONS]).unwrap();
    assert_eq!(reopened.agent(a.id).unwrap(), Some(a));
    reopened
        .with_connection(|c| {
            let mode: String = c
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            Ok(())
        })
        .unwrap();
    drop(reopened);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn migrations_merge_roles_validate_first_and_rollback_failed_step() {
    const A: &[Migration] = &[Migration {
        version: 1002,
        name: "a",
        sql: "CREATE TABLE ordering (n INTEGER); INSERT INTO ordering VALUES (1002);",
    }];
    const B: &[Migration] = &[
        Migration {
            version: 2002,
            name: "b2",
            sql: "INSERT INTO ordering VALUES (2002);",
        },
        Migration {
            version: 2001,
            name: "b1",
            sql: "INSERT INTO ordering VALUES (2001);",
        },
    ];
    let mut a = MIGRATIONS.to_vec();
    a.extend_from_slice(A);
    let store = SqliteStore::in_memory(&[&a, B]).unwrap();
    store
        .with_connection(|c| {
            let rows: Vec<i64> = c
                .prepare("SELECT n FROM ordering ORDER BY rowid")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert_eq!(rows, [1002, 2001, 2002]);
            Ok(())
        })
        .unwrap();
    assert_eq!(store.migrate(&[&a, B]).unwrap(), 0);
    for bad in [
        Migration {
            version: 1000,
            name: "duplicate",
            sql: "",
        },
        Migration {
            version: 999,
            name: "low",
            sql: "",
        },
        Migration {
            version: 3000,
            name: "high",
            sql: "",
        },
    ] {
        let empty = SqliteStore::in_memory(&[]).unwrap();
        assert_eq!(
            empty.migrate(&[MIGRATIONS, &[bad]]).unwrap_err().code,
            ErrorCode::InvalidParams
        );
        empty
            .with_connection(|c| {
                let count: i64 = c
                    .query_row(
                        "SELECT count(*) FROM sqlite_master WHERE name='agents'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(count, 0);
                let count: i64 = c
                    .query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, 0);
                Ok(())
            })
            .unwrap();
    }
    let failed = Migration {
        version: 1003,
        name: "bad sql",
        sql: "CREATE TABLE rolled_back (id TEXT); INSERT INTO missing VALUES (1);",
    };
    assert!(store.migrate(&[&[failed]]).is_err());
    store
        .with_connection(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='rolled_back'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn agents_roundtrip_conflicts_updates_and_soft_deletion() {
    let store = store();
    let mut a = agent();
    assert_eq!(store.agent(a.id).unwrap(), None);
    assert_eq!(
        store.update_agent(&a).unwrap_err().code,
        ErrorCode::NotFound
    );
    store.insert_agent(&a).unwrap();
    assert_eq!(store.agents().unwrap(), [a.clone()]);
    assert_eq!(store.agent_by_slug(&a.slug).unwrap(), Some(a.clone()));
    assert_eq!(
        store.insert_agent(&a).unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut b = agent();
    assert_eq!(
        store.insert_agent(&b).unwrap_err().code,
        ErrorCode::Conflict
    );
    b.slug = "other".into();
    store.insert_agent(&b).unwrap();
    b.slug = a.slug.clone();
    assert_eq!(
        store.update_agent(&b).unwrap_err().code,
        ErrorCode::Conflict
    );
    a.name = "Updated".into();
    a.detected = None;
    a.last_activity_at = None;
    store.update_agent(&a).unwrap();
    assert_eq!(store.agent(a.id).unwrap(), Some(a.clone()));
    store.delete_agent(a.id, at(10)).unwrap();
    store.delete_agent(a.id, at(11)).unwrap();
    assert_eq!(store.agent(a.id).unwrap(), None);
    assert_eq!(store.agent_by_slug(&a.slug).unwrap(), None);
    assert_eq!(store.agents().unwrap().len(), 1);
    assert_eq!(
        store.update_agent(&a).unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        store.insert_agent(&a).unwrap_err().code,
        ErrorCode::Conflict
    );
    let mut replacement = agent();
    replacement.slug = a.slug;
    store.insert_agent(&replacement).unwrap();
    store
        .with_connection(|c| {
            let deleted: String = c
                .query_row(
                    "SELECT deleted_at FROM agents WHERE id=?",
                    [a.id.to_string()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(deleted, "2026-11-09T00:00:10.000000000Z");
            Ok(())
        })
        .unwrap();
    let _: Arc<dyn Store> = Arc::new(store);
}

#[test]
fn sessions_roundtrip_and_replace_by_agent_or_run() {
    let store = store();
    let mut s = session(AgentId::generate());
    let run = RunId::generate();
    assert_eq!(store.main_session(s.agent_id).unwrap(), None);
    assert_eq!(store.fork_session(run).unwrap(), None);
    store.put_main_session(&s).unwrap();
    store.put_fork_session(run, &s).unwrap();
    assert_eq!(store.main_session(s.agent_id).unwrap(), Some(s.clone()));
    assert_eq!(store.fork_session(run).unwrap(), Some(s.clone()));
    s.session_id = SessionId("replacement".into());
    s.last_activity_at = None;
    store.put_main_session(&s).unwrap();
    store.put_fork_session(run, &s).unwrap();
    assert_eq!(store.main_session(s.agent_id).unwrap(), Some(s.clone()));
    assert_eq!(store.fork_session(run).unwrap(), Some(s.clone()));
    let second = RunId::generate();
    store.put_fork_session(second, &s).unwrap();
    assert_eq!(store.fork_session(second).unwrap(), Some(s));
}

#[test]
fn schedules_roundtrip_filter_runs_page_and_delete() {
    let store = store();
    let mut s = schedule(AgentId::generate());
    let other = schedule(AgentId::generate());
    store.put_schedule(&s).unwrap();
    store.put_schedule(&other).unwrap();
    assert_eq!(store.schedule(s.id).unwrap(), Some(s.clone()));
    assert_eq!(store.schedules(None).unwrap().len(), 2);
    for second in [5, 1, 3] {
        let run = ScheduleRun {
            id: ScheduleRunId::generate(),
            schedule_id: s.id,
            planned_at: at(second),
            started_at: Some(at(second)),
            finished_at: Some(at(second + 1)),
            result: Some(ScheduleRunResult::MissedRan),
        };
        store.put_schedule_run(&run).unwrap();
        assert!(store.schedule_runs(s.id, None, 10).unwrap().contains(&run));
        s.last_run = Some(run);
    }
    s.paused = false;
    store.put_schedule(&s).unwrap();
    assert_eq!(store.schedules(Some(s.agent_id)).unwrap(), [s.clone()]);
    assert!(
        store
            .schedules(Some(AgentId::generate()))
            .unwrap()
            .is_empty()
    );
    let mut run = s.last_run.clone().unwrap();
    run.result = None;
    store.put_schedule_run(&run).unwrap();
    assert_eq!(store.schedule_runs(s.id, Some(at(5)), 1).unwrap(), [run]);
    assert_eq!(
        store
            .schedule_runs(s.id, None, 10)
            .unwrap()
            .iter()
            .map(|r| r.planned_at)
            .collect::<Vec<_>>(),
        [at(5), at(3), at(1)]
    );
    assert!(store.schedule_runs(s.id, None, 0).unwrap().is_empty());
    assert!(store.schedule_runs(other.id, None, 10).unwrap().is_empty());
    store.delete_schedule(s.id).unwrap();
    store.delete_schedule(s.id).unwrap();
    assert_eq!(store.schedule(s.id).unwrap(), None);
    assert_eq!(store.schedule_runs(s.id, None, 10).unwrap().len(), 3);
}

#[test]
fn attention_roundtrip_oldest_first_resolves_all_matching_open_items() {
    let store = store();
    let target = AttentionTarget::Agent {
        agent_id: AgentId::generate(),
    };
    let mut items = Vec::new();
    for second in [4, 1, 2] {
        let item = Attention {
            id: AttentionId::generate(),
            kind: AttentionKind::AgentError,
            target,
            subject: "error".into(),
            at: at(second),
            resolved_at: None,
        };
        store.put_attention(&item).unwrap();
        items.push(item);
    }
    items.sort_by_key(|i| i.at);
    assert_eq!(store.open_attention().unwrap(), items);
    let other = Attention {
        id: AttentionId::generate(),
        target: AttentionTarget::Schedule {
            schedule_id: ScheduleId::generate(),
        },
        ..items[0].clone()
    };
    store.put_attention(&other).unwrap();
    let mut closed = items[0].clone();
    closed.resolved_at = Some(at(6));
    store.put_attention(&closed).unwrap();
    let mut expected = items[1..].to_vec();
    for item in &mut expected {
        item.resolved_at = Some(at(10));
    }
    assert_eq!(store.resolve_attention(&target, at(10)).unwrap(), expected);
    assert_eq!(store.open_attention().unwrap(), [other]);
    assert!(store.resolve_attention(&target, at(11)).unwrap().is_empty());
}

#[test]
fn activity_roundtrip_combined_filters_before_limit_upsert_and_delete() {
    let store = store();
    let a = AgentId::generate();
    let m = MemberId::generate();
    let mut items = Vec::new();
    for second in [1, 3, 5] {
        let item = Activity {
            id: ActivityId::generate(),
            kind: ActivityKind::AgentStarted,
            actor: Some(m),
            agent_id: Some(a),
            target: Some(AttentionTarget::Agent { agent_id: a }),
            at: at(second),
            summary: "start".into(),
        };
        store.record_activity(&item).unwrap();
        items.push(item);
    }
    let wrong_member = Activity {
        id: ActivityId::generate(),
        actor: None,
        at: at(8),
        ..items[0].clone()
    };
    let wrong_agent = Activity {
        id: ActivityId::generate(),
        agent_id: None,
        at: at(9),
        ..items[0].clone()
    };
    store.record_activity(&wrong_member).unwrap();
    store.record_activity(&wrong_agent).unwrap();
    assert_eq!(
        store.activity(Some(a), Some(m), Some(at(5)), 1).unwrap(),
        [items[1].clone()]
    );
    assert_eq!(store.activity(Some(a), None, None, 10).unwrap().len(), 4);
    assert_eq!(store.activity(None, Some(m), None, 10).unwrap().len(), 4);
    assert_eq!(
        store.activity(None, None, None, 10).unwrap()[0],
        wrong_agent
    );
    assert!(store.activity(None, None, None, 0).unwrap().is_empty());
    items[2].summary = "updated".into();
    store.record_activity(&items[2]).unwrap();
    assert_eq!(
        store.activity(Some(a), Some(m), None, 1).unwrap(),
        [items[2].clone()]
    );
    store
        .delete_activity(&[items[0].id, items[1].id, ActivityId::generate()])
        .unwrap();
    store.delete_activity(&[]).unwrap();
    assert_eq!(
        store.activity(Some(a), Some(m), None, 10).unwrap(),
        [items[2].clone()]
    );
}

#[test]
fn settings_defaults_roundtrip_and_replace() {
    let store = store();
    assert_eq!(store.settings().unwrap(), Settings::default());
    let mut settings = Settings {
        language: Some(Language::Ko),
        ..Settings::default()
    };
    settings.notifications.enabled = false;
    settings.notifications.muted_until = Some(at(10));
    store.put_settings(&settings).unwrap();
    assert_eq!(store.settings().unwrap(), settings);
    store.put_settings(&Settings::default()).unwrap();
    assert_eq!(store.settings().unwrap(), Settings::default());
}

#[test]
fn paging_compares_fractional_timestamps_as_instants() {
    let store = store();
    let times = [
        "2026-11-09T00:00:01Z",
        "2026-11-09T00:00:01.000000001Z",
        "2026-11-09T00:00:01.1Z",
    ];
    for time in times {
        store
            .record_activity(&Activity {
                id: ActivityId::generate(),
                kind: ActivityKind::AgentStopped,
                actor: None,
                agent_id: None,
                target: None,
                at: time.parse().unwrap(),
                summary: time.into(),
            })
            .unwrap();
    }
    let page = store
        .activity(None, None, Some(times[2].parse().unwrap()), 10)
        .unwrap();
    assert_eq!(
        page.iter().map(|a| a.summary.as_str()).collect::<Vec<_>>(),
        [times[1], times[0]]
    );
}
