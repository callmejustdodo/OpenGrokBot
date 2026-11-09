//! The owner's side of the request endpoints for the peer server (TASK-023,
//! #179): [`PeerRequests`] implements [`RequestEndpoints`] over [`Requests`], and
//! [`forward_updates`] sends every change to its requester's events connection.
//!
//! formationd wires both together: the server calls [`PeerRequests`] for
//! `POST /v1/requests`, `GET /v1/requests/{id}`, and
//! `POST /v1/requests/{id}/cancel`, and [`forward_updates`] carries
//! [`Requests::subscribe`] onto the [`PeerEventHub`]. [`PeerRequests`] does not
//! publish anything itself, so a request is published once per change, by the
//! forwarder, whichever path changed it (a teammate, the owner, or the executor).
//!
//! What reaches the requester is masked exactly once
//! ([`for_requester`], #165): [`Requests::subscribe`] and the [`endpoints`]
//! already mask, so only the requests read here straight from the store
//! ([`Requests::owned`]) are masked here.

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use chrono::TimeDelta;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use formation_proto::common::{MemberId, RequestId, Timestamp};
use formation_proto::peer::{PeerEvent, PeerRequestCreate};
use formation_proto::policy::Caller;
use formation_proto::request::WorkRequest;

use super::Requests;
use super::endpoints;
use super::outgoing::for_requester;
use super::queue::now;
use crate::peer::api::{PeerEventHub, RequestEndpoints, not_member};
use crate::ports::Result;

/// How long a request that ended stays in what a reconnecting requester is sent
/// again. The owner's store has no per-member "last delivered" mark, so
/// [`PeerRequests::requests_to_resend`] cannot tell which final updates a member
/// missed; it re-sends every one that ended within this window instead. A week
/// covers a teammate whose Mac was off over a weekend or a holiday; sending one
/// they already have is harmless (they keep the newest by `updated_at`), and
/// past the window they can still read it with `GET /v1/requests/{id}`.
pub const RESEND_FINAL_WITHIN: TimeDelta = TimeDelta::days(7);

/// After the forwarder falls behind, how far before the last update it sent it
/// looks again, so a wall clock stepping back a little loses nothing.
const LAG_SLACK: TimeDelta = TimeDelta::seconds(5);

/// [`RequestEndpoints`] for the peer server, over this Mac's [`Requests`].
pub struct PeerRequests {
    requests: Arc<Requests>,
}

impl PeerRequests {
    pub fn new(requests: Arc<Requests>) -> Self {
        Self { requests }
    }
}

#[async_trait]
impl RequestEndpoints for PeerRequests {
    /// The member, their agent, and the chain come from `caller`, the identity
    /// the server checked with the policy, rather than from the body. A repeated
    /// (requester, idempotency key) returns the original
    /// ([`Requests::create`]).
    async fn create(&self, caller: Caller, request: PeerRequestCreate) -> Result<WorkRequest> {
        let Caller::Member {
            member_id,
            via_agent,
            chain,
        } = caller
        else {
            return Err(not_member(
                "only a teammate sends work requests over the peer protocol",
            ));
        };
        let body = PeerRequestCreate {
            via_agent,
            chain,
            ..request
        };
        endpoints::create(&self.requests, member_id, body).await
    }

    async fn get(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        endpoints::get(&self.requests, member, id)
    }

    async fn cancel(&self, member: MemberId, id: RequestId) -> Result<WorkRequest> {
        endpoints::cancel(&self.requests, member, id)
    }

    /// Every request of `member` that has not ended, and every one that ended
    /// within [`RESEND_FINAL_WITHIN`], oldest change first, masked for them.
    async fn requests_to_resend(&self, member: MemberId) -> Result<Vec<WorkRequest>> {
        let since = now() - RESEND_FINAL_WITHIN;
        let mut out: Vec<WorkRequest> = self
            .requests
            .owned()?
            .into_iter()
            .filter(|request| request.requester == member && worth_sending(request, since))
            .map(|request| for_requester(&request))
            .collect();
        out.sort_by_key(|request| request.updated_at);
        Ok(out)
    }
}

/// Publishes every change to a request on this Mac to its requester as
/// [`PeerEvent::RequestUpdate`], until `requests` is dropped. formationd spawns
/// it once, next to the peer server.
///
/// It subscribes before returning, so nothing changed after the call is missed.
/// When it falls behind and the channel drops updates, it cannot tell which, so
/// it re-sends every request that has not ended and every one that changed since
/// shortly before the last update it sent; the requester keeps the newest by
/// `updated_at`, so a repeat is harmless. It holds `requests` weakly, so it does
/// not keep the queue alive.
pub fn forward_updates(requests: &Arc<Requests>, hub: PeerEventHub) -> JoinHandle<()> {
    let mut updates = requests.subscribe();
    let requests: Weak<Requests> = Arc::downgrade(requests);
    let subscribed = now();
    tokio::spawn(async move {
        let mut last_sent: Option<Timestamp> = None;
        loop {
            match updates.recv().await {
                // Already masked by `Requests`.
                Ok(request) => {
                    last_sent = last_sent.max(Some(request.updated_at));
                    publish(&hub, request);
                }
                Err(RecvError::Lagged(_)) => {
                    let Some(requests) = requests.upgrade() else {
                        return;
                    };
                    let since = last_sent.unwrap_or(subscribed) - LAG_SLACK;
                    // A store error leaves the requester to catch up by reading the
                    // request or on their next reconnect.
                    for request in requests.owned().unwrap_or_default() {
                        if worth_sending(&request, since) {
                            publish(&hub, for_requester(&request));
                        }
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    })
}

/// Not ended yet, or changed at or after `since`.
fn worth_sending(request: &WorkRequest, since: Timestamp) -> bool {
    !request.status.is_final() || request.updated_at >= since
}

fn publish(hub: &PeerEventHub, request: WorkRequest) {
    hub.publish_to(
        request.requester,
        PeerEvent::RequestUpdate {
            request: Box::new(request),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use tokio::sync::broadcast;
    use tokio::time::timeout;

    use formation_proto::common::{AgentId, ChainId, ErrorCode};
    use formation_proto::policy::Chain;
    use formation_proto::request::RequestStatus;

    use super::super::testing::{MemoryCollab, ScriptedPolicy};
    use super::*;
    use crate::ask::mask::Masker;
    use crate::peer::api::{Audience, HubMessage};
    use crate::ports::{RequestQueue, RequestRepo, RunProgress};
    use crate::testkit::RecordingEventSink;

    struct Owner {
        agent: AgentId,
        collab: Arc<MemoryCollab>,
        requests: Arc<Requests>,
        endpoints: PeerRequests,
        hub: PeerEventHub,
    }

    fn owner() -> Owner {
        let agent = AgentId::generate();
        let collab = Arc::new(MemoryCollab::new(MemberId::generate()));
        collab.share_agent(agent, true);
        let requests = Arc::new(Requests::new(
            collab.clone(),
            Arc::new(ScriptedPolicy::default()),
            Arc::new(RecordingEventSink::default()),
        ));
        Owner {
            agent,
            collab,
            endpoints: PeerRequests::new(requests.clone()),
            requests,
            hub: PeerEventHub::new(),
        }
    }

    fn member(member_id: MemberId) -> Caller {
        Caller::Member {
            member_id,
            via_agent: Some("web".into()),
            chain: Chain {
                id: ChainId::generate(),
                depth: 1,
            },
        }
    }

    fn body(agent_id: AgentId, key: &str) -> PeerRequestCreate {
        PeerRequestCreate {
            request_id: RequestId::generate(),
            agent_id,
            prompt: "Add token expiry tests".into(),
            idempotency_key: key.into(),
            via_agent: None,
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
        }
    }

    /// Built at run time so secret scanners do not flag the test string.
    fn secret() -> String {
        format!("{}{}", "ghp_", "AbCdEfGhIjKlMnOpQrStUvWxYz012345")
    }

    /// Every request update the hub carried, until it is quiet, with its audience.
    async fn drain(rx: &mut broadcast::Receiver<HubMessage>) -> Vec<(Audience, WorkRequest)> {
        let mut out = Vec::new();
        while let Ok(Ok(message)) = timeout(Duration::from_millis(200), rx.recv()).await {
            if let HubMessage::Event {
                audience,
                event: PeerEvent::RequestUpdate { request },
            } = message
            {
                out.push((audience, *request));
            }
        }
        out
    }

    #[tokio::test]
    async fn create_takes_the_member_from_the_caller_and_replays_by_idempotency_key() {
        let owner = owner();
        let teammate = MemberId::generate();
        let caller = member(teammate);
        let first = owner
            .endpoints
            .create(caller.clone(), body(owner.agent, "k1"))
            .await
            .unwrap();
        assert_eq!(first.requester, teammate);
        assert_eq!(first.requester_agent.as_deref(), Some("web"));
        assert_eq!(first.chain.depth, 1);
        assert_eq!(first.status, RequestStatus::Queued);

        let again = owner
            .endpoints
            .create(caller, body(owner.agent, "k1"))
            .await
            .unwrap();
        assert_eq!(again.id, first.id);
        assert_eq!(owner.collab.requests().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_owner_cannot_create_over_the_peer_protocol() {
        let owner = owner();
        let refused = owner
            .endpoints
            .create(Caller::Owner, body(owner.agent, "k1"))
            .await
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::NotMember);
        assert!(owner.collab.requests().unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_requester_reads_or_cancels() {
        let owner = owner();
        let teammate = MemberId::generate();
        let created = owner
            .endpoints
            .create(member(teammate), body(owner.agent, "k1"))
            .await
            .unwrap();
        let stranger = MemberId::generate();
        let read = owner.endpoints.get(stranger, created.id).await;
        assert_eq!(read.unwrap_err().code, ErrorCode::NotFound);
        let cancel = owner.endpoints.cancel(stranger, created.id).await;
        assert_eq!(cancel.unwrap_err().code, ErrorCode::NotFound);

        let read = owner.endpoints.get(teammate, created.id).await.unwrap();
        assert_eq!(read.status, RequestStatus::Queued);
        let canceled = owner.endpoints.cancel(teammate, created.id).await.unwrap();
        assert_eq!(canceled.status, RequestStatus::Canceled);
    }

    #[tokio::test]
    async fn resends_open_and_recently_ended_requests_of_that_member_masked() {
        let owner = owner();
        let teammate = MemberId::generate();
        let open = owner
            .endpoints
            .create(member(teammate), body(owner.agent, "open"))
            .await
            .unwrap();

        // Ended now, with a secret in the result.
        let mut recent = owner
            .endpoints
            .create(member(teammate), body(owner.agent, "recent"))
            .await
            .unwrap();
        recent.status = RequestStatus::Completed;
        recent.result = Some(format!("Pushed with {}.", secret()));
        recent.updated_at = now();
        owner.collab.put_request(&recent).unwrap();

        // Ended long ago.
        let mut old = owner
            .endpoints
            .create(member(teammate), body(owner.agent, "old"))
            .await
            .unwrap();
        old.status = RequestStatus::Canceled;
        old.updated_at = now() - RESEND_FINAL_WITHIN - TimeDelta::hours(1);
        owner.collab.put_request(&old).unwrap();

        // Another member's.
        owner
            .endpoints
            .create(member(MemberId::generate()), body(owner.agent, "other"))
            .await
            .unwrap();

        let resent = owner.endpoints.requests_to_resend(teammate).await.unwrap();
        let ids: Vec<RequestId> = resent.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![open.id, recent.id]);
        let result = resent[1].result.as_deref().unwrap();
        assert!(!result.contains(&secret()), "{result}");
    }

    #[tokio::test]
    async fn forwards_each_change_to_its_requester_masked_once() {
        let owner = owner();
        let mut hub = owner.hub.subscribe();
        let forwarder = forward_updates(&owner.requests, owner.hub.clone());
        let teammate = MemberId::generate();
        let created = owner
            .endpoints
            .create(member(teammate), body(owner.agent, "k1"))
            .await
            .unwrap();

        let original = format!("Pushed with token {}.", secret());
        let item = owner.requests.next(owner.agent).await.unwrap();
        owner
            .requests
            .report(item.id, RunProgress::Started)
            .await
            .unwrap();
        owner
            .requests
            .report(
                item.id,
                RunProgress::Completed {
                    result: Some(original.clone()),
                },
            )
            .await
            .unwrap();

        let updates = drain(&mut hub).await;
        assert!(
            updates
                .iter()
                .all(|(audience, r)| *audience == Audience::Member(teammate) && r.id == created.id)
        );
        let statuses: Vec<RequestStatus> = updates.iter().map(|(_, r)| r.status).collect();
        assert_eq!(
            statuses,
            vec![
                RequestStatus::Received,
                RequestStatus::Queued,
                RequestStatus::Running,
                RequestStatus::Completed,
            ]
        );
        let sent = updates[3].1.result.clone().unwrap();
        assert_eq!(sent, Masker::new().mask(&original));
        assert!(!sent.contains(&secret()));

        drop(owner.endpoints);
        drop(owner.requests);
        timeout(Duration::from_secs(1), forwarder)
            .await
            .expect("the forwarder stops with the queue")
            .unwrap();
    }

    #[tokio::test]
    async fn a_forwarder_that_fell_behind_sends_the_current_requests_again() {
        let owner = owner();
        let mut hub = owner.hub.subscribe();
        let _forwarder = forward_updates(&owner.requests, owner.hub.clone());
        // On this single-threaded runtime the forwarder does not run until the
        // test waits, so 2 updates per request overflow the 256-update channel.
        let mut latest = HashMap::new();
        for n in 0..150 {
            let teammate = MemberId::generate();
            let created = owner
                .endpoints
                .create(member(teammate), body(owner.agent, &format!("k{n}")))
                .await
                .unwrap();
            latest.insert(created.id, (teammate, created.status));
        }

        let updates = drain(&mut hub).await;
        // The 256 updates the channel kept, then one per request from the resync:
        // more than the 300 sent, so the forwarder did fall behind.
        assert!(updates.len() > 300, "{}", updates.len());
        let mut seen = HashMap::new();
        for (audience, request) in updates {
            assert_eq!(audience, Audience::Member(request.requester));
            seen.insert(request.id, (request.requester, request.status));
        }
        assert_eq!(seen, latest);
    }
}
