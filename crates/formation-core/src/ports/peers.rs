//! Owner: B. The client side of the peer protocol: talking to teammates' formationd.

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::ask::Question;
use formation_proto::common::{MemberId, QuestionId, RequestId};
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerEvent, PeerJoin, PeerJoinAccepted, PeerQuestionAction,
    PeerRequestCreate, SharedAgentView,
};
use formation_proto::request::WorkRequest;
use formation_proto::team::Member;

use super::Result;

#[async_trait]
pub trait Peers: Send + Sync {
    /// Joins the team at the creator's tailnet address (carried in the invitation link).
    async fn join(&self, creator_address: &str, join: PeerJoin) -> Result<PeerJoinAccepted>;

    async fn roster(&self, member: MemberId) -> Result<Vec<Member>>;

    async fn agents(&self, member: MemberId) -> Result<Vec<SharedAgentView>>;

    /// `peer_offline` means the question waits in the outbox and is sent when the
    /// teammate's Mac is back (FR-016).
    async fn ask(&self, member: MemberId, ask: PeerAsk) -> Result<PeerAskAccepted>;

    /// Cancel, refresh, or route a question that lives on the owner's Mac.
    async fn question_action(
        &self,
        member: MemberId,
        question_id: QuestionId,
        action: PeerQuestionAction,
    ) -> Result<Question>;

    /// `peer_offline` as for [`Peers::ask`].
    async fn create_request(
        &self,
        member: MemberId,
        request: PeerRequestCreate,
    ) -> Result<WorkRequest>;

    async fn request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest>;

    async fn cancel_request(&self, member: MemberId, id: RequestId) -> Result<WorkRequest>;

    /// Presence, question, request, and roster updates from every teammate.
    fn subscribe(&self) -> broadcast::Receiver<PeerEvent>;
}
