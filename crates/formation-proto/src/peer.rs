//! Owner: B (collaboration core).
//!
//! The peer protocol between formationd instances over the tailnet (tech spec,
//! "Peer protocol"), plus the teammate views shown in the app.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::agent::{AgentState, Avatar, HarnessKind};
use crate::ask::{AskTarget, Question, RouteChoice};
use crate::common::{AgentId, ApiError, MemberId, QuestionId, RequestId, Timestamp, commands};
use crate::policy::Chain;
use crate::request::WorkRequest;
use crate::team::{Member, TeamId};

/// Assumed default peer port (tech spec).
pub const PEER_PORT: u16 = 7413;
pub const API_PREFIX: &str = "/v1";

pub const PATH_AGENTS: &str = "/v1/agents";
pub const PATH_ASK: &str = "/v1/ask";
pub const PATH_QUESTION_ACTION: &str = "/v1/questions/{id}/action";
pub const PATH_REQUESTS: &str = "/v1/requests";
pub const PATH_REQUEST: &str = "/v1/requests/{id}";
pub const PATH_REQUEST_CANCEL: &str = "/v1/requests/{id}/cancel";
/// The roster, served by every member's Mac; the creator's is authoritative.
pub const PATH_TEAM: &str = "/v1/team";
/// Served by the creator's Mac only. The one call accepted from a tailnet node that is
/// not a member yet.
pub const PATH_TEAM_JOIN: &str = "/v1/team/join";
pub const PATH_EVENTS: &str = "/v1/events";

/// A teammate's shared agent as the caller may see it. Never sent for private agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SharedAgentView {
    pub owner: MemberId,
    pub agent_id: AgentId,
    /// For CLI and MCP addresses such as `dohyeon/api`.
    pub slug: String,
    pub name: String,
    pub role: String,
    pub harness: HarnessKind,
    pub avatar: Avatar,
    pub state: AgentState,
    pub last_activity_at: Option<Timestamp>,
    pub accepts_requests: bool,
}

/// `GET /v1/agents`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentsResponse {
    pub agents: Vec<SharedAgentView>,
}

/// `POST /v1/ask`. The asker's Mac picks `question_id`, so a retry from the outbox
/// returns the same question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerAsk {
    pub question_id: QuestionId,
    /// The first question of a follow-up thread.
    pub thread_id: Option<QuestionId>,
    pub target: AskTarget,
    pub text: String,
    pub via_agent: Option<String>,
    pub chain: Chain,
}

/// `202 Accepted` for `POST /v1/ask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerAskAccepted {
    pub question_id: QuestionId,
}

/// `POST /v1/questions/{id}/action`: what the asker does to a question that lives
/// on the owner's Mac. Returns the [`Question`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case")]
#[ts(export)]
pub enum PeerQuestionAction {
    Cancel,
    Refresh,
    Route { choice: RouteChoice },
}

/// `POST /v1/requests`. Returns the [`WorkRequest`]; a repeated idempotency key
/// returns the original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerRequestCreate {
    pub request_id: RequestId,
    pub agent_id: AgentId,
    pub prompt: String,
    pub idempotency_key: String,
    pub via_agent: Option<String>,
    pub chain: Chain,
}

/// `POST /v1/team/join`. The creator's Mac checks the code and records the caller's
/// Tailscale WhoIs (login and node, never from this body) with the GitHub login.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerJoin {
    pub code: String,
    /// From `gh api user` on the joiner's Mac in the MVP.
    pub github_login: String,
    pub display_name: String,
}

/// Reply to `POST /v1/team/join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerJoinAccepted {
    /// The joiner's new member ID.
    pub member_id: MemberId,
    pub members: Vec<Member>,
    /// The team joined; matches the invitation link's.
    #[serde(default)]
    pub team_id: Option<TeamId>,
    /// The roster version of `members`.
    #[serde(default)]
    #[ts(type = "number")]
    pub version: u64,
}

/// `GET /v1/team`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RosterResponse {
    pub members: Vec<Member>,
    /// Starts at 1 when the team is created and goes up by one on every roster change
    /// on the creator's Mac. A Mac applies a roster only when its version is newer.
    #[serde(default)]
    #[ts(type = "number")]
    pub version: u64,
}

/// A member came online or went offline, or their shared agents changed. Also the
/// payload of the app's `peer.presence` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PresenceEvent {
    pub member_id: MemberId,
    pub online: bool,
    pub last_seen_at: Option<Timestamp>,
    /// The member's shared agents as last seen.
    pub agents: Vec<SharedAgentView>,
}

/// Messages on `WS /v1/events`, filtered to what the caller may see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum PeerEvent {
    Presence {
        presence: PresenceEvent,
    },
    QuestionUpdate {
        question: Box<Question>,
    },
    /// The owner deleted their copy (if owner deletes reach the asker; to confirm).
    QuestionRemoved {
        question_id: QuestionId,
    },
    RequestUpdate {
        request: Box<WorkRequest>,
    },
    /// The roster changed (join, leave, removal). Sent by the creator's Mac; a removed
    /// member loses access on every Mac once it arrives.
    Roster {
        members: Vec<Member>,
        /// As [`RosterResponse::version`].
        #[serde(default)]
        #[ts(type = "number")]
        version: u64,
    },
}

/// Error body on every peer endpoint: `{ "error": { "code", "message" } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerErrorBody {
    pub error: ApiError,
}

// ---------------------------------------------------------------------------
// Commands (local socket)
// ---------------------------------------------------------------------------

/// Shared agents of one teammate, or of every teammate when `member_id` is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeammateAgents {
    pub member_id: Option<MemberId>,
}

commands! {
    TeammateAgents => "teammate_agents" -> Vec<SharedAgentView>;
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::testutil::t0;

    pub fn shared_agent(owner: MemberId) -> SharedAgentView {
        SharedAgentView {
            owner,
            agent_id: AgentId::generate(),
            slug: "web".into(),
            name: "Web".into(),
            role: "Frontend developer".into(),
            harness: HarnessKind::ClaudeCode,
            avatar: Avatar {
                shape: "square".into(),
                color: "green".into(),
            },
            state: AgentState::Working,
            last_activity_at: Some(t0()),
            accepts_requests: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::shared_agent;
    use super::*;
    use crate::common::{ChainId, ErrorCode};
    use crate::request::samples::work_request;
    use crate::team::samples::team;
    use crate::testutil::{roundtrip, t0};

    #[test]
    fn peer_payloads_round_trip() {
        let owner = MemberId::generate();
        let view = shared_agent(owner);
        roundtrip(&AgentsResponse {
            agents: vec![view.clone()],
        });
        let chain = Chain {
            id: ChainId::generate(),
            depth: 0,
        };
        roundtrip(&PeerAsk {
            question_id: QuestionId::generate(),
            thread_id: None,
            target: AskTarget::Agent {
                member_id: owner,
                agent_id: view.agent_id,
            },
            text: "Is the login API done?".into(),
            via_agent: None,
            chain,
        });
        let json = roundtrip(&PeerQuestionAction::Route {
            choice: RouteChoice::Agent {
                agent_id: view.agent_id,
            },
        });
        assert_eq!(json["action"], "route");
        assert_eq!(json["choice"]["kind"], "agent");
        roundtrip(&PeerRequestCreate {
            request_id: RequestId::generate(),
            agent_id: view.agent_id,
            prompt: "Add a health endpoint".into(),
            idempotency_key: "k1".into(),
            via_agent: Some("web".into()),
            chain,
        });
        let json = roundtrip(&PeerEvent::Presence {
            presence: PresenceEvent {
                member_id: owner,
                online: false,
                last_seen_at: Some(t0()),
                agents: vec![view.clone()],
            },
        });
        assert_eq!(json["type"], "presence");
        let json = roundtrip(&PeerEvent::RequestUpdate {
            request: Box::new(work_request(owner, view.agent_id)),
        });
        assert_eq!(json["type"], "request_update");
    }

    #[test]
    fn team_payloads_round_trip() {
        let team = team();
        roundtrip(&PeerJoin {
            code: "7F3K-Q9PX".into(),
            github_login: "baekdusan".into(),
            display_name: "Baekdusan".into(),
        });
        roundtrip(&PeerJoinAccepted {
            member_id: team.members[1].id,
            members: team.members.clone(),
            team_id: Some(TeamId::generate()),
            version: 2,
        });
        roundtrip(&RosterResponse {
            members: team.members.clone(),
            version: 2,
        });
        let json = roundtrip(&PeerEvent::Roster {
            members: team.members.clone(),
            version: 2,
        });
        assert_eq!(json["type"], "roster");
        assert_eq!(json["version"], 2);

        // Additive fields: replies written before the roster version still decode.
        let old = serde_json::json!({ "members": [], "type": "roster" });
        assert_eq!(
            serde_json::from_value::<PeerEvent>(old).unwrap(),
            PeerEvent::Roster {
                members: vec![],
                version: 0
            }
        );
        let old = serde_json::json!({ "member_id": team.members[1].id, "members": [] });
        let accepted: PeerJoinAccepted = serde_json::from_value(old).unwrap();
        assert_eq!((accepted.team_id, accepted.version), (None, 0));
    }

    #[test]
    fn error_body_has_the_documented_shape() {
        let body = PeerErrorBody {
            error: ApiError::new(ErrorCode::AgentNotShared, "not shared"),
        };
        assert_eq!(
            roundtrip(&body),
            serde_json::json!({"error": {"code": "agent_not_shared", "message": "not shared"}})
        );
    }
}
