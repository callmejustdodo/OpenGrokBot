//! Owner: B (collaboration core).
//!
//! Work requests (FR-014, FR-017) and the per-agent queue that feeds the main
//! session: owner messages, teammates' requests, and schedule runs share one ordered
//! queue per agent.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{
    AgentId, ApiError, MemberId, MessageId, QuestionId, RequestId, ScheduleId, Timestamp, commands,
};
use crate::policy::Chain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RequestStatus {
    Received,
    AwaitingApproval,
    Queued,
    Running,
    Completed,
    Denied,
    Failed,
    Canceled,
    /// An approval went unanswered for 24 hours.
    Expired,
}

impl RequestStatus {
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Denied | Self::Failed | Self::Canceled | Self::Expired
        )
    }
}

/// A work request, as both the requester's and the owner's Mac keep it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkRequest {
    pub id: RequestId,
    pub requester: MemberId,
    pub requester_agent: Option<String>,
    pub chain: Chain,
    pub owner: MemberId,
    pub agent_id: AgentId,
    pub prompt: String,
    /// Unique per requester.
    pub idempotency_key: String,
    pub status: RequestStatus,
    /// The agent's final reply, when completed.
    pub result: Option<String>,
    pub error: Option<ApiError>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Where a queued message came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum QueueSource {
    Owner,
    Request {
        request_id: RequestId,
        requester: MemberId,
    },
    Schedule {
        schedule_id: ScheduleId,
    },
}

/// One message waiting for the agent's next turn (shown as a queued chip, SC-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueueItem {
    pub id: MessageId,
    pub agent_id: AgentId,
    pub source: QueueSource,
    pub text: String,
    pub queued_at: Timestamp,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RequestCreate {
    pub owner: MemberId,
    pub agent_id: AgentId,
    pub prompt: String,
    pub idempotency_key: String,
    /// Built from selected messages of this side conversation (FR-014).
    pub from_question: Option<QuestionId>,
    /// Set by the CLI and MCP server when one of my agents sends the request; see
    /// [`Ask::from_agent`](crate::ask::Ask::from_agent).
    pub from_agent: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RequestGet {
    pub request_id: RequestId,
}

/// Cancels before the request starts running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RequestCancel {
    pub request_id: RequestId,
}

commands! {
    RequestCreate => "request_create" -> WorkRequest;
    RequestGet => "request_get" -> WorkRequest;
    RequestCancel => "request_cancel" -> WorkRequest;
}

/// `request.update`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RequestUpdateEvent {
    pub request: WorkRequest,
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::common::ChainId;
    use crate::testutil::t0;

    pub fn work_request(owner: MemberId, agent_id: AgentId) -> WorkRequest {
        WorkRequest {
            id: RequestId::generate(),
            requester: MemberId::generate(),
            requester_agent: Some("web".into()),
            chain: Chain {
                id: ChainId::generate(),
                depth: 1,
            },
            owner,
            agent_id,
            prompt: "Add a health endpoint".into(),
            idempotency_key: "k1".into(),
            status: RequestStatus::AwaitingApproval,
            result: None,
            error: None,
            created_at: t0(),
            updated_at: t0(),
        }
    }

    pub fn queue_item(agent_id: AgentId) -> QueueItem {
        QueueItem {
            id: MessageId::generate(),
            agent_id,
            source: QueueSource::Request {
                request_id: RequestId::generate(),
                requester: MemberId::generate(),
            },
            text: "Add a health endpoint".into(),
            queued_at: t0(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;
    use crate::testutil::roundtrip;

    #[test]
    fn requests_round_trip() {
        let (owner, agent_id) = (MemberId::generate(), AgentId::generate());
        let json = roundtrip(&RequestUpdateEvent {
            request: work_request(owner, agent_id),
        });
        assert_eq!(json["request"]["status"], "awaiting_approval");
        assert_eq!(
            roundtrip(&queue_item(agent_id))["source"]["kind"],
            "request"
        );
        roundtrip(&RequestCreate {
            owner,
            agent_id,
            prompt: "Add a health endpoint".into(),
            idempotency_key: "k1".into(),
            from_question: Some(QuestionId::generate()),
            from_agent: None,
        });
        assert!(RequestStatus::Expired.is_final());
        assert!(!RequestStatus::Queued.is_final());
    }
}
