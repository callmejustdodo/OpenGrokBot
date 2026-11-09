//! What the peer server hands each call to. The `team`, `ask`, and `requests`
//! modules implement these, so their rules stay in those modules; the server
//! only identifies the caller, applies the policy checks named below, and maps
//! results and errors to HTTP.

use async_trait::async_trait;

use formation_proto::ask::Question;
use formation_proto::common::{MemberId, QuestionId, RequestId};
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerJoin, PeerJoinAccepted, PeerQuestionAction, PeerRequestCreate,
};
use formation_proto::policy::Caller;
use formation_proto::request::WorkRequest;
use formation_proto::team::WhoIs;

use crate::ports::Result;

/// `POST /v1/team/join`, served by the creator's Mac (TASK-017, `team`). Another
/// Mac answers `not_found`.
#[async_trait]
pub trait JoinEndpoint: Send + Sync {
    /// `whois` is the joiner's Tailscale identity, to record with the GitHub
    /// login. The joiner is not a member yet: no policy check has run; the
    /// invitation code is the check.
    async fn join(&self, whois: WhoIs, join: PeerJoin) -> Result<PeerJoinAccepted>;
}

/// `POST /v1/ask` and `POST /v1/questions/{id}/action` (TASK-019, `ask`).
#[async_trait]
pub trait AskEndpoints: Send + Sync {
    /// The caller is a member, the target belongs to this Mac, and
    /// [`Policy::check_ask`](crate::ports::Policy::check_ask) passed.
    ///
    /// A `question_id` this asker already sent is an outbox replay: return it
    /// without asking again. Publish the question to the asker with
    /// [`PeerEventHub::publish_to`](super::PeerEventHub::publish_to) once it is
    /// accepted and on every change after.
    async fn ask(&self, caller: Caller, ask: PeerAsk) -> Result<PeerAskAccepted>;

    /// Only the asker may act on their question (`not_found` for anyone else).
    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question>;

    /// Questions to send the member again each time their events connection
    /// opens: at least every one not final, and every one that became final
    /// while the member was not connected, so an update published meanwhile is
    /// not lost. Sending one the asker already has is harmless; the asker keeps
    /// the newest by `updated_at`.
    async fn questions_to_resend(&self, member: MemberId) -> Result<Vec<Question>> {
        let _ = member;
        Ok(Vec::new())
    }
}

/// `POST /v1/requests`, `GET /v1/requests/{id}`, `POST /v1/requests/{id}/cancel`
/// (TASK-023, `requests`).
#[async_trait]
pub trait RequestEndpoints: Send + Sync {
    /// The caller is a member and
    /// [`Policy::check_request`](crate::ports::Policy::check_request) passed. A
    /// repeated (requester, idempotency key) returns the original. Publish the
    /// request to the requester on every change, as for questions.
    async fn create(&self, caller: Caller, request: PeerRequestCreate) -> Result<WorkRequest>;

    /// Only the requester may read or cancel their request (`not_found` otherwise).
    async fn get(&self, member: MemberId, id: RequestId) -> Result<WorkRequest>;

    async fn cancel(&self, member: MemberId, id: RequestId) -> Result<WorkRequest>;

    /// Requests to send the member again on reconnect, chosen as for
    /// [`AskEndpoints::questions_to_resend`].
    async fn requests_to_resend(&self, member: MemberId) -> Result<Vec<WorkRequest>> {
        let _ = member;
        Ok(Vec::new())
    }
}
