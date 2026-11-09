use std::sync::Arc;

use formation_core::{
    notify::{AttentionNotifier, monitor},
    ports::Notifier,
    testkit::{MemoryStore, RecordingEventSink},
};
use formation_proto::{
    agent::{AgentState, AgentStateEvent, AttentionKind, AttentionTarget},
    common::{AgentId, Seq},
    events::{Event, EventEnvelope},
};
use tokio::sync::broadcast;

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
