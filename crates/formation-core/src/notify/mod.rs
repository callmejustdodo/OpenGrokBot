//! Persisted attention items and monitoring of the harness's agent state events.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use formation_proto::agent::{AgentState, Attention, AttentionKind, AttentionTarget, NotifyEvent};
use formation_proto::common::{ApiError, AttentionId, ErrorCode};
use formation_proto::events::{Event, EventEnvelope};
use tokio::sync::broadcast;

use crate::ports::{AttentionRepo, EventSink, Notifier, Result};

pub struct AttentionNotifier<R: ?Sized> {
    repo: Arc<R>,
    events: Arc<dyn EventSink>,
    updates: Mutex<()>,
}

impl<R: AttentionRepo + Send + Sync + ?Sized> AttentionNotifier<R> {
    pub fn new(repo: Arc<R>, events: Arc<dyn EventSink>) -> Self {
        Self {
            repo,
            events,
            updates: Mutex::new(()),
        }
    }
}

#[async_trait]
impl<R: AttentionRepo + Send + Sync + ?Sized> Notifier for AttentionNotifier<R> {
    async fn raise(
        &self,
        kind: AttentionKind,
        target: AttentionTarget,
        subject: String,
    ) -> Result<Attention> {
        let _update = self.updates.lock().unwrap();
        if let Some(item) = self
            .repo
            .open_attention()?
            .into_iter()
            .find(|item| item.kind == kind && item.target == target)
        {
            return Ok(item);
        }
        let item = Attention {
            id: AttentionId::generate(),
            kind,
            target,
            subject,
            at: SystemTime::now().into(),
            resolved_at: None,
        };
        self.repo.put_attention(&item)?;
        self.events.publish(Event::Notify(NotifyEvent {
            item: item.clone(),
            alert: true,
        }));
        Ok(item)
    }

    async fn resolve(&self, target: AttentionTarget) -> Result<()> {
        let _update = self.updates.lock().unwrap();
        for item in self
            .repo
            .resolve_attention(&target, SystemTime::now().into())?
        {
            self.events
                .publish(Event::Notify(NotifyEvent { item, alert: false }));
        }
        Ok(())
    }

    async fn inbox(&self) -> Result<Vec<Attention>> {
        self.repo.open_attention()
    }
}

/// Watches a receiver from `EventBus::subscribe`. The harness owns the stalled
/// threshold and unexpected-exit detection. Returns on stream closure or error.
pub async fn monitor(
    mut events: broadcast::Receiver<EventEnvelope>,
    notifier: Arc<dyn Notifier>,
) -> Result<()> {
    loop {
        let envelope = match events.recv().await {
            Ok(envelope) => envelope,
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
            Err(error) => return Err(ApiError::new(ErrorCode::Internal, error.to_string())),
        };
        let Event::AgentState(event) = envelope.event else {
            continue;
        };
        let target = AttentionTarget::Agent {
            agent_id: event.agent_id,
        };
        let kind = match event.state {
            AgentState::Stalled => AttentionKind::AgentStalled,
            AgentState::Error => AttentionKind::AgentError,
            AgentState::Working | AgentState::Idle => {
                notifier.resolve(target).await?;
                continue;
            }
            _ => continue,
        };
        notifier
            .raise(
                kind,
                target,
                event.reason.unwrap_or_else(|| event.agent_id.to_string()),
            )
            .await?;
    }
}
