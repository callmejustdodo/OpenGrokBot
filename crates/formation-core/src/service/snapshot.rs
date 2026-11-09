use std::sync::Arc;

use formation_proto::common::{ApiError, ErrorCode, Seq};
use formation_proto::events::EventEnvelope;
use formation_proto::harness::Conversation;
use formation_proto::snapshot::Snapshot;
use formation_proto::team::Team;
use tokio::sync::broadcast;

use crate::harness::dispatch::HarnessDispatcher;
use crate::ports::{CollabStore, Harness, Peers, Result, Store};

use super::EventBus;

#[derive(Clone)]
pub struct SnapshotBuilder {
    store: Arc<dyn Store>,
    collab: Arc<dyn CollabStore>,
    harness: Arc<dyn Harness>,
    peers: Arc<dyn Peers>,
    /// When set, the snapshot lists every registered harness instead of `harness`.
    dispatcher: Option<Arc<HarnessDispatcher>>,
}

impl SnapshotBuilder {
    pub fn new(
        store: Arc<dyn Store>,
        collab: Arc<dyn CollabStore>,
        harness: Arc<dyn Harness>,
        peers: Arc<dyn Peers>,
    ) -> Self {
        Self {
            store,
            collab,
            harness,
            peers,
            dispatcher: None,
        }
    }

    /// Lists every harness of `dispatcher` in the snapshot, in registration order.
    pub fn with_harnesses(mut self, dispatcher: Arc<HarnessDispatcher>) -> Self {
        self.dispatcher = Some(dispatcher);
        self
    }

    /// Subscribe to the bus first, then read the last Seq as snapshot.seq, then
    /// read state. Send the client only events with seq > snapshot.seq. An event
    /// published while state is read may appear both in the snapshot and after
    /// it; events replace state, so applying it again is harmless.
    pub async fn capture(
        &self,
        bus: &EventBus,
    ) -> Result<(Snapshot, broadcast::Receiver<EventEnvelope>)> {
        let receiver = bus.subscribe();
        let seq = bus.last_seq();
        let builder = self.clone();
        let (mut snapshot, conversations) =
            tokio::task::spawn_blocking(move || builder.read_stored(seq))
                .await
                .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))??;
        for mut conversation in conversations {
            if conversation.session_id.is_some() {
                let Ok(page) = self
                    .harness
                    .conversation(conversation.agent_id, None, None, 100)
                    .await
                else {
                    continue;
                };
                conversation.items = page.items;
                conversation.has_more = page.has_more;
                conversation.stream_seq = page.stream_seq;
            }
            snapshot.conversations.push(conversation);
        }
        if let Some(team) = &snapshot.team {
            for member in &team.members {
                if member.id != team.me
                    && let Ok(agents) = self.peers.agents(member.id).await
                {
                    snapshot.teammate_agents.extend(agents);
                }
            }
        }
        match &self.dispatcher {
            Some(dispatcher) => snapshot.harnesses.extend(dispatcher.statuses().await),
            None => snapshot.harnesses.push(self.harness.status().await),
        }
        Ok((snapshot, receiver))
    }

    fn read_stored(&self, seq: Seq) -> Result<(Snapshot, Vec<Conversation>)> {
        let team = self
            .collab
            .me()?
            .map(|me| {
                Ok::<_, formation_proto::common::ApiError>(Team {
                    me,
                    members: self.collab.members()?,
                    invitation: None,
                })
            })
            .transpose()?;
        let agents = self.store.agents()?;
        let mut conversations = Vec::new();
        for agent in &agents {
            conversations.push(Conversation {
                agent_id: agent.id,
                session_id: self
                    .store
                    .main_session(agent.id)?
                    .map(|session| session.session_id),
                items: vec![],
                has_more: false,
                queue: self.collab.queue_items(agent.id)?,
                stream_seq: 0,
            });
        }
        Ok((
            Snapshot {
                seq,
                team,
                agents,
                shares: self.collab.shares()?,
                schedules: self.store.schedules(None)?,
                conversations: vec![],
                teammate_agents: vec![],
                inbox: self.store.open_attention()?,
                questions: self.collab.questions()?,
                requests: self.collab.requests()?,
                approvals: self.collab.approvals(None)?,
                rules: self.collab.rules(None)?,
                harnesses: vec![],
                settings: self.store.settings()?,
            },
            conversations,
        ))
    }
}
