//! The requester's side: `request_create`, `request_get`, and `request_cancel` on
//! the local socket. Requests go to the owner's Mac through the [`Peers`] port, and
//! this Mac keeps a copy that follows the owner's updates (FR-014, FR-017).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;

use formation_proto::common::{ChainId, Command, ErrorCode, MemberId, RequestId, RunId};
use formation_proto::events::Event;
use formation_proto::peer::PeerRequestCreate;
use formation_proto::policy::{Caller, Chain};
use formation_proto::request::{
    METHODS, QueueSource, RequestCancel, RequestCreate, RequestGet, RequestStatus,
    RequestUpdateEvent, WorkRequest,
};

use super::Requests;
use super::queue::{check_chain_depth, error, now, request_not_found, require_text};
use super::status::check_transition;
use crate::ports::{
    CollabStore, CommandCall, CommandHandler, EventSink, Harness, Peers, Result, Store,
};

/// Sends this Mac's work requests and serves the request commands. Requests to this
/// Mac's own agents are read from the store and canceled through [`Requests`].
pub struct RequestClient {
    collab: Arc<dyn CollabStore>,
    peers: Arc<dyn Peers>,
    harness: Arc<dyn Harness>,
    agents: Arc<dyn Store>,
    events: Arc<dyn EventSink>,
    owner_side: Arc<Requests>,
}

impl RequestClient {
    pub fn new(
        collab: Arc<dyn CollabStore>,
        peers: Arc<dyn Peers>,
        harness: Arc<dyn Harness>,
        agents: Arc<dyn Store>,
        events: Arc<dyn EventSink>,
        owner_side: Arc<Requests>,
    ) -> Self {
        Self {
            collab,
            peers,
            harness,
            agents,
            events,
            owner_side,
        }
    }

    /// Stores the request and sends it to the owner's Mac, which returns its copy.
    ///
    /// - A repeated idempotency key returns the stored request; the same key with
    ///   another target or prompt is a `conflict`. A stored request the owner never
    ///   acknowledged (still `received`) is sent again, which the owner's Mac
    ///   deduplicates, so it is delivered once and runs once.
    /// - With `from_run`, one of this Mac's agents sends the request: it continues
    ///   the chain of the request that run serves, one level deeper, or starts a
    ///   chain at depth 1 for the owner's own work. Deeper than
    ///   [`MAX_CHAIN_DEPTH`](formation_proto::policy::MAX_CHAIN_DEPTH) is
    ///   `rate_limited` (FR-019), checked here and again by the owner's Mac.
    /// - `peer_offline` leaves the request `received`; the outbox sends it when the
    ///   owner's Mac is back (FR-016). Any other refusal marks it `failed` with that
    ///   error and returns the error.
    pub async fn create(&self, command: RequestCreate) -> Result<WorkRequest> {
        require_text("prompt", &command.prompt)?;
        require_text("idempotency_key", &command.idempotency_key)?;
        let me = self.me()?;
        if command.owner == me {
            return Err(error(
                ErrorCode::InvalidParams,
                "send your own agent a message instead of a work request",
            ));
        }
        let (chain, via_agent) = self.chain(command.from_run).await?;
        check_chain_depth(chain.depth)?;

        let at = now();
        let request = WorkRequest {
            id: RequestId::generate(),
            requester: me,
            requester_agent: via_agent,
            chain,
            owner: command.owner,
            agent_id: command.agent_id,
            prompt: command.prompt,
            idempotency_key: command.idempotency_key,
            status: RequestStatus::Received,
            result: None,
            error: None,
            created_at: at,
            updated_at: at,
        };
        let (stored, inserted) = self.collab.insert_request(&request)?;
        if inserted {
            self.publish(&stored);
        } else if stored.owner != request.owner
            || stored.agent_id != request.agent_id
            || stored.prompt != request.prompt
        {
            return Err(error(
                ErrorCode::Conflict,
                "the idempotency key was already used for a different request",
            ));
        } else if stored.status != RequestStatus::Received {
            return Ok(stored);
        }
        self.send(stored).await
    }

    /// Sends a stored `received` request to its owner and keeps the owner's answer.
    async fn send(&self, stored: WorkRequest) -> Result<WorkRequest> {
        let body = PeerRequestCreate {
            request_id: stored.id,
            agent_id: stored.agent_id,
            prompt: stored.prompt.clone(),
            idempotency_key: stored.idempotency_key.clone(),
            via_agent: stored.requester_agent.clone(),
            chain: stored.chain,
        };
        match self.peers.create_request(stored.owner, body).await {
            Ok(remote) => self.accept_remote(stored.owner, remote),
            Err(refusal) if refusal.code == ErrorCode::PeerOffline => Err(refusal),
            Err(refusal) => {
                // Re-read: a concurrent retry with the same key may already hold the
                // owner's copy, which a stale `failed` must not overwrite.
                let mut failed = self.collab.request(stored.id)?.unwrap_or(stored);
                if failed.status != RequestStatus::Received {
                    return Err(refusal);
                }
                check_transition(failed.status, RequestStatus::Failed)?;
                failed.status = RequestStatus::Failed;
                failed.error = Some(refusal.clone());
                failed.updated_at = now().max(failed.updated_at);
                self.collab.put_request(&failed)?;
                self.publish(&failed);
                Err(refusal)
            }
        }
    }

    /// The request as the owner's Mac has it now, or this Mac's copy when the owner
    /// is offline or has not received it yet.
    pub async fn get(&self, id: RequestId) -> Result<WorkRequest> {
        let me = self.me()?;
        let local = self.collab.request(id)?.ok_or_else(request_not_found)?;
        if local.owner == me {
            return Ok(local);
        }
        if local.requester != me {
            return Err(request_not_found());
        }
        match self.peers.request(local.owner, id).await {
            Ok(remote) => self.accept_remote(local.owner, remote),
            Err(failure)
                if matches!(failure.code, ErrorCode::PeerOffline | ErrorCode::NotFound) =>
            {
                Ok(local)
            }
            Err(failure) => Err(failure),
        }
    }

    /// Cancels before the run starts, on the owner's Mac.
    pub async fn cancel(&self, id: RequestId) -> Result<WorkRequest> {
        let me = self.me()?;
        let local = self.collab.request(id)?.ok_or_else(request_not_found)?;
        if local.owner == me {
            return self.owner_side.cancel(None, id);
        }
        if local.requester != me {
            return Err(request_not_found());
        }
        let remote = self.peers.cancel_request(local.owner, id).await?;
        self.accept_remote(local.owner, remote)
    }

    /// Applies a `PeerEvent::RequestUpdate` from `from`. Updates for requests this
    /// Mac did not send, or sent to someone else, are ignored.
    pub fn apply_update(&self, from: MemberId, request: WorkRequest) -> Result<()> {
        let me = self.me()?;
        match self.collab.request(request.id)? {
            Some(local) if local.requester == me && local.owner == from => {
                self.accept_remote(from, request).map(drop)
            }
            _ => Ok(()),
        }
    }

    fn me(&self) -> Result<MemberId> {
        self.collab
            .me()?
            .ok_or_else(|| error(ErrorCode::NotMember, "this Mac has not joined a team"))
    }

    /// The chain and acting agent for a request sent from `from_run`.
    async fn chain(&self, from_run: Option<RunId>) -> Result<(Chain, Option<String>)> {
        let Some(run_id) = from_run else {
            return Ok((new_chain(0), None));
        };
        let run = self
            .harness
            .run(run_id)
            .await
            .ok_or_else(|| error(ErrorCode::NotFound, "no agent run has this ID"))?;
        let via_agent = self.agents.agent(run.agent_id)?.map(|agent| agent.slug);
        let chain = match run.item.source {
            QueueSource::Request { request_id, .. } => {
                let parent = self
                    .collab
                    .request(request_id)?
                    .ok_or_else(request_not_found)?;
                Chain {
                    id: parent.chain.id,
                    depth: parent.chain.depth.saturating_add(1),
                }
            }
            QueueSource::Owner | QueueSource::Schedule { .. } => new_chain(1),
        };
        Ok((chain, via_agent))
    }

    /// Stores the owner's copy unless this Mac already has a newer one.
    fn accept_remote(&self, from: MemberId, remote: WorkRequest) -> Result<WorkRequest> {
        let local = self
            .collab
            .request(remote.id)?
            .ok_or_else(request_not_found)?;
        if remote.owner != from
            || remote.owner != local.owner
            || remote.requester != local.requester
            || remote.agent_id != local.agent_id
        {
            return Err(error(
                ErrorCode::Conflict,
                "the owner's copy does not match this request",
            ));
        }
        if !supersedes(&remote, &local) {
            return Ok(local);
        }
        self.collab.put_request(&remote)?;
        self.publish(&remote);
        Ok(remote)
    }

    fn publish(&self, request: &WorkRequest) {
        self.events
            .publish(Event::RequestUpdate(RequestUpdateEvent {
                request: request.clone(),
            }));
    }
}

#[async_trait]
impl CommandHandler for RequestClient {
    fn methods(&self) -> Vec<&'static str> {
        METHODS.to_vec()
    }

    /// Teammates reach requests through the peer endpoints, never through commands.
    async fn handle(&self, call: CommandCall) -> Result<serde_json::Value> {
        if call.caller != Caller::Owner {
            return Err(error(
                ErrorCode::Denied,
                "request commands are for this Mac's owner",
            ));
        }
        let method = call.method.as_str();
        if method == RequestCreate::METHOD {
            respond(self.create(params(&call)?).await)
        } else if method == RequestGet::METHOD {
            let RequestGet { request_id } = params(&call)?;
            respond(self.get(request_id).await)
        } else if method == RequestCancel::METHOD {
            let RequestCancel { request_id } = params(&call)?;
            respond(self.cancel(request_id).await)
        } else {
            Err(error(
                ErrorCode::InvalidParams,
                format!("{method} is not a request command"),
            ))
        }
    }
}

/// Whether the owner's copy is newer than this Mac's. Statuses only move forward,
/// so they order the copies; `updated_at` orders copies at the same step, which
/// all come from the owner's clock except this Mac's own `received` copy.
fn supersedes(remote: &WorkRequest, local: &WorkRequest) -> bool {
    fn step(status: RequestStatus) -> u8 {
        match status {
            RequestStatus::Received => 0,
            RequestStatus::Queued => 1,
            RequestStatus::Running | RequestStatus::AwaitingApproval => 2,
            RequestStatus::Completed
            | RequestStatus::Denied
            | RequestStatus::Failed
            | RequestStatus::Canceled
            | RequestStatus::Expired => 3,
        }
    }
    if local.status.is_final() || remote == local {
        return false;
    }
    match step(remote.status).cmp(&step(local.status)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => {
            local.status == RequestStatus::Received || remote.updated_at >= local.updated_at
        }
        std::cmp::Ordering::Less => false,
    }
}

fn new_chain(depth: u8) -> Chain {
    Chain {
        id: ChainId::generate(),
        depth,
    }
}

/// Omitted or `null` params read as `{}`, as on the rest of the local socket.
fn params<C: DeserializeOwned>(call: &CommandCall) -> Result<C> {
    let params = match &call.params {
        serde_json::Value::Null => serde_json::json!({}),
        params => params.clone(),
    };
    serde_json::from_value(params)
        .map_err(|failure| error(ErrorCode::InvalidParams, failure.to_string()))
}

fn respond<T: Serialize>(result: Result<T>) -> Result<serde_json::Value> {
    serde_json::to_value(result?).map_err(|failure| error(ErrorCode::Internal, failure.to_string()))
}
