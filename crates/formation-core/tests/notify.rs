use std::sync::Arc;
use std::time::{Duration, SystemTime};

use formation_core::{
    notify::{AttentionNotifier, monitor, reconcile},
    ports::{AgentRepo, Notifier, SettingsRepo},
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{
    agent::{
        Agent, AgentState, AgentStateEvent, AttentionKind, AttentionTarget, Avatar, HarnessKind,
    },
    common::{AgentId, Seq},
    events::{Event, EventEnvelope},
    snapshot::Settings,
};
use tokio::sync::broadcast;

fn agent(slug: &str, state: AgentState) -> Agent {
    Agent {
        id: AgentId::generate(),
        slug: slug.into(),
        name: slug.into(),
        role: "developer".into(),
        harness: HarnessKind::ClaudeCode,
        folder_path: slug.into(),
        repositories: vec![],
        avatar: Avatar {
            shape: "circle".into(),
            color: "blue".into(),
        },
        state,
        files_revision: "1".into(),
        created_at: SystemTime::now().into(),
        last_activity_at: None,
        detected: None,
    }
}

fn alerts(events: &RecordingEventSink) -> Vec<bool> {
    events
        .events()
        .into_iter()
        .map(|(_, event)| match event {
            Event::Notify(event) => event.alert,
            _ => panic!("expected notify"),
        })
        .collect()
}

#[tokio::test]
async fn attention_is_persisted_deduplicated_and_resolved_by_target() {
    let store = Arc::new(MemoryStore::default());
    let events = Arc::new(RecordingEventSink::default());
    let notifier = AttentionNotifier::new(store.clone(), events.clone());
    let target = AttentionTarget::Agent {
        agent_id: AgentId::generate(),
    };
    let other_target = AttentionTarget::Agent {
        agent_id: AgentId::generate(),
    };
    let stalled = notifier
        .raise(AttentionKind::AgentStalled, target, "Backend".into())
        .await
        .unwrap();
    assert_eq!(
        notifier
            .raise(AttentionKind::AgentStalled, target, "Backend".into())
            .await
            .unwrap(),
        stalled
    );
    let error = notifier
        .raise(AttentionKind::AgentError, target, "Runtime failed".into())
        .await
        .unwrap();
    let other = notifier
        .raise(AttentionKind::AgentStalled, other_target, "Frontend".into())
        .await
        .unwrap();
    // A new adapter sees the persisted inbox.
    assert_eq!(
        AttentionNotifier::new(store, events.clone())
            .inbox()
            .await
            .unwrap(),
        vec![stalled.clone(), error.clone(), other.clone()]
    );
    assert_eq!(events.events().len(), 3);
    notifier.resolve(target).await.unwrap();
    notifier.resolve(target).await.unwrap();
    assert_eq!(notifier.inbox().await.unwrap(), vec![other]);
    let recorded = events.events();
    assert_eq!(recorded.len(), 5);
    for (_, event) in &recorded[..3] {
        let Event::Notify(event) = event else {
            panic!("expected notify");
        };
        assert!(event.alert);
        assert!(event.item.resolved_at.is_none());
    }
    for ((_, event), original) in recorded[3..].iter().zip([stalled.clone(), error]) {
        let Event::Notify(event) = event else {
            panic!("expected notify");
        };
        assert!(!event.alert);
        assert_eq!(event.item.id, original.id);
        assert!(event.item.resolved_at.unwrap() >= original.at);
    }
    let raised_again = notifier
        .raise(AttentionKind::AgentStalled, target, "Backend".into())
        .await
        .unwrap();
    assert_ne!(raised_again.id, stalled.id);
}

#[tokio::test]
async fn monitor_alerts_on_stall_and_error_and_resolves_on_recovery() {
    let events = Arc::new(RecordingEventSink::default());
    let notifier = Arc::new(AttentionNotifier::new(
        Arc::new(MemoryStore::default()),
        events.clone(),
    ));
    let (sender, receiver) = broadcast::channel(16);
    let agent_id = AgentId::generate();
    let states = [
        AgentState::Idle,
        AgentState::WaitingForApproval,
        AgentState::Stalled,
        AgentState::Stalled,
        AgentState::Working,
        AgentState::Error,
        AgentState::Error,
        AgentState::Idle,
        AgentState::Stopped,
    ];
    for (index, state) in states.into_iter().enumerate() {
        sender
            .send(EventEnvelope {
                seq: Seq(index as u64 + 1),
                at: std::time::SystemTime::now().into(),
                event: Event::AgentState(AgentStateEvent {
                    agent_id,
                    state,
                    reason: Some("Backend".into()),
                    exit_code: None,
                }),
            })
            .unwrap();
    }
    drop(sender);
    monitor(receiver, notifier.clone()).await.unwrap();
    assert!(notifier.inbox().await.unwrap().is_empty());
    let recorded = events.events();
    assert_eq!(recorded.len(), 4);
    for ((_, event), (kind, alert)) in recorded.iter().zip([
        (AttentionKind::AgentStalled, true),
        (AttentionKind::AgentStalled, false),
        (AttentionKind::AgentError, true),
        (AttentionKind::AgentError, false),
    ]) {
        let Event::Notify(event) = event else {
            panic!("expected notify");
        };
        assert_eq!(event.item.kind, kind);
        assert_eq!(event.item.target, AttentionTarget::Agent { agent_id });
        assert_eq!(event.item.subject, "Backend");
        assert_eq!(event.alert, alert);
        assert_eq!(event.item.resolved_at.is_none(), alert);
    }
}

#[tokio::test]
async fn muted_raise_is_stored_without_alert() {
    let store = Arc::new(MemoryStore::default());
    let events = Arc::new(RecordingEventSink::default());
    let notifier = AttentionNotifier::new(store.clone(), events.clone());
    let mut settings = Settings::default();
    settings.notifications.muted_until =
        Some((SystemTime::now() + Duration::from_secs(3600)).into());
    store.put_settings(&settings).unwrap();
    let muted = notifier
        .raise(
            AttentionKind::AgentStalled,
            AttentionTarget::Agent {
                agent_id: AgentId::generate(),
            },
            "Backend".into(),
        )
        .await
        .unwrap();
    settings.notifications.muted_until = None;
    settings.notifications.enabled = false;
    store.put_settings(&settings).unwrap();
    let disabled = notifier
        .raise(
            AttentionKind::AgentError,
            AttentionTarget::Agent {
                agent_id: AgentId::generate(),
            },
            "Frontend".into(),
        )
        .await
        .unwrap();
    settings.notifications.enabled = true;
    settings.notifications.muted_until = Some((SystemTime::now() - Duration::from_secs(60)).into());
    store.put_settings(&settings).unwrap();
    let expired = notifier
        .raise(
            AttentionKind::AgentError,
            AttentionTarget::Agent {
                agent_id: AgentId::generate(),
            },
            "Docs".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        notifier.inbox().await.unwrap(),
        vec![muted, disabled, expired]
    );
    assert_eq!(alerts(&events), vec![false, false, true]);
}

#[tokio::test]
async fn reconcile_raises_for_stalled_agents_and_resolves_recovered_ones() {
    let store = Arc::new(MemoryStore::default());
    let events = Arc::new(RecordingEventSink::default());
    let notifier = AttentionNotifier::new(store.clone(), events.clone());
    let stalled = agent("stalled", AgentState::Stalled);
    let failed = agent("failed", AgentState::Error);
    let recovered = agent("recovered", AgentState::Idle);
    let stopped = agent("stopped", AgentState::Stopped);
    for agent in [&stalled, &failed, &recovered, &stopped] {
        store.insert_agent(agent).unwrap();
    }
    notifier
        .raise(
            AttentionKind::AgentStalled,
            AttentionTarget::Agent {
                agent_id: recovered.id,
            },
            "recovered".into(),
        )
        .await
        .unwrap();
    reconcile(store.as_ref(), &notifier).await.unwrap();
    let inbox = notifier.inbox().await.unwrap();
    let open: Vec<_> = inbox
        .iter()
        .map(|item| (item.kind, item.target, item.subject.as_str()))
        .collect();
    assert_eq!(
        open,
        vec![
            (
                AttentionKind::AgentStalled,
                AttentionTarget::Agent {
                    agent_id: stalled.id
                },
                "stalled"
            ),
            (
                AttentionKind::AgentError,
                AttentionTarget::Agent {
                    agent_id: failed.id
                },
                "failed"
            ),
        ]
    );
    // Reconciling again changes nothing.
    reconcile(store.as_ref(), &notifier).await.unwrap();
    assert_eq!(notifier.inbox().await.unwrap(), inbox);
    // The original raise, then stalled, failed and the recovered resolution.
    assert_eq!(alerts(&events), vec![true, true, true, false]);
}
