//! Handlers for the request endpoints of the peer protocol (tech spec, "Peer
//! protocol"), for the peer server (TASK-018) to call once it has mapped the
//! connection's Tailscale WhoIs to a member with
//! [`Policy::authenticate`](crate::ports::Policy::authenticate).
//!
//! | Endpoint | Handler |
//! | --- | --- |
//! | `POST /v1/requests` | [`create`] |
//! | `GET /v1/requests/{id}` | [`get`] |
//! | `POST /v1/requests/{id}/cancel` | [`cancel`] |
//!
//! Each returns the [`WorkRequest`] for the body, with its result and error text
//! masked for the requester ([`for_requester`]), or an [`ApiError`] for the
//! `{ "error": { "code", "message" } }` body
//! ([`PeerErrorBody`](formation_proto::peer::PeerErrorBody)). The peer server picks
//! the HTTP status from the code. [`parse_request_id`] reads the `{id}` path
//! segment.

use formation_proto::common::{ApiError, ErrorCode, MemberId, RequestId};
use formation_proto::peer::PeerRequestCreate;
use formation_proto::request::WorkRequest;

use super::Requests;
use super::outgoing::for_requester;
use crate::ports::Result;

/// `POST /v1/requests`. A repeated idempotency key returns the original request.
pub async fn create(
    requests: &Requests,
    member: MemberId,
    body: PeerRequestCreate,
) -> Result<WorkRequest> {
    requests
        .create(member, body)
        .await
        .map(|r| for_requester(&r))
}

/// `GET /v1/requests/{id}`. Only the member who sent the request can read it.
pub fn get(requests: &Requests, member: MemberId, id: RequestId) -> Result<WorkRequest> {
    requests.get(member, id).map(|r| for_requester(&r))
}

/// `POST /v1/requests/{id}/cancel`. Only before the run starts.
pub fn cancel(requests: &Requests, member: MemberId, id: RequestId) -> Result<WorkRequest> {
    requests.cancel(Some(member), id).map(|r| for_requester(&r))
}

/// The `{id}` path segment; `invalid_params` when it is not a request ID.
pub fn parse_request_id(segment: &str) -> Result<RequestId> {
    serde_json::from_value(serde_json::Value::String(segment.to_owned())).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidParams,
            format!("{segment:?} is not a request ID"),
        )
    })
}
