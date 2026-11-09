use async_trait::async_trait;
use formation_core::ports::*;
use formation_proto::{ask::*, common::*, peer::*, request::*, team::*};
use tokio::sync::broadcast;

// The `Peers` port without a tailnet: no teammate is reachable. B's `PeerClient`
// needs the peer server and a roster directory, which formationd does not run yet.
pub struct NoTeam {
    events: broadcast::Sender<PeerEvent>,
}

impl NoTeam {
    pub fn new() -> Self {
        Self {
            events: broadcast::channel(16).0,
        }
    }
}

fn unavailable<T>() -> Result<T> {
    Err(ApiError::new(
        ErrorCode::PeerOffline,
        "this Mac does not reach teammates yet (#107)",
    ))
}

#[async_trait]
impl Peers for NoTeam {
    async fn join(&self, _creator_address: &str, _join: PeerJoin) -> Result<PeerJoinAccepted> {
        unavailable()
    }
    async fn roster(&self, _member: MemberId) -> Result<Vec<Member>> {
        Ok(vec![])
    }
    async fn agents(&self, _member: MemberId) -> Result<Vec<SharedAgentView>> {
        Ok(vec![])
    }
    async fn ask(&self, _member: MemberId, _ask: PeerAsk) -> Result<PeerAskAccepted> {
        unavailable()
    }
    async fn question_action(
        &self,
        _member: MemberId,
        _question_id: QuestionId,
        _action: PeerQuestionAction,
    ) -> Result<Question> {
        unavailable()
    }
    async fn create_request(
        &self,
        _member: MemberId,
        _request: PeerRequestCreate,
    ) -> Result<WorkRequest> {
        unavailable()
    }
    async fn request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> {
        unavailable()
    }
    async fn cancel_request(&self, _member: MemberId, _id: RequestId) -> Result<WorkRequest> {
        unavailable()
    }
    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        self.events.subscribe()
    }
}
