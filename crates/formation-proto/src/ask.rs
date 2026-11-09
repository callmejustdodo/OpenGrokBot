//! Owner: B (collaboration core).
//!
//! Questions to a teammate or a teammate's agent, routing, and answers
//! (FR-010 to FR-013, tech spec "Ask pipeline").

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{AgentId, AnswerId, ApiError, MemberId, QuestionId, Timestamp, commands};

/// Answers built from a transcript entry older than this are marked `possibly_stale`.
pub const STALE_AFTER_MINUTES: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum AskTarget {
    /// Formation picks the agent (FR-010).
    Person { member_id: MemberId },
    Agent {
        member_id: MemberId,
        agent_id: AgentId,
    },
}

impl AskTarget {
    pub fn owner(&self) -> MemberId {
        match self {
            Self::Person { member_id } | Self::Agent { member_id, .. } => *member_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum QuestionStatus {
    Received,
    Answering,
    /// Routing was not confident; the asker picks from `candidates` (D-027).
    Unsure,
    /// The owner's Mac is offline; the question waits in the outbox.
    WaitingOffline,
    /// The asker sent it to the person instead.
    SentToOwner,
    Answered,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RoutingCandidate {
    pub agent_id: AgentId,
    pub name: String,
    pub role: String,
    /// 0.0 to 1.0.
    pub score: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ExcerptRole {
    User,
    Assistant,
}

/// A conversation passage an answer used. Masked for secrets before it leaves the Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Excerpt {
    pub role: ExcerptRole,
    pub text: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Answer {
    pub id: AnswerId,
    /// `None` when the owner answered in person.
    pub agent_id: Option<AgentId>,
    pub text: String,
    /// Empty when the agent's visibility is `answer` (D-025).
    pub excerpts: Vec<Excerpt>,
    /// Time of the newest transcript entry used.
    pub as_of: Timestamp,
    pub possibly_stale: bool,
    pub answered_at: Timestamp,
}

/// A question, as both the asker's and the owner's Mac keep it (D-029).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Question {
    pub id: QuestionId,
    /// The first question of the thread; equals `id` for a new thread.
    pub thread_id: QuestionId,
    pub asker: MemberId,
    pub target: AskTarget,
    pub text: String,
    pub status: QuestionStatus,
    /// Filled when `status` is `unsure`.
    pub candidates: Vec<RoutingCandidate>,
    /// The agent that answers, once known.
    pub agent_id: Option<AgentId>,
    pub answer: Option<Answer>,
    pub error: Option<ApiError>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Ask {
    pub target: AskTarget,
    pub text: String,
    /// Continue a thread (follow-ups continue the forked session).
    pub thread_id: Option<QuestionId>,
    /// Set by the CLI and MCP server when one of my agents asks (from
    /// `FORMATION_AGENT_ID`, which formationd sets for each run). formationd then
    /// continues the chain of that agent's running turn, so FR-019 limits apply.
    pub from_agent: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionCancel {
    pub question_id: QuestionId,
}

/// Ask again for a fresh answer (for a possibly stale one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionRefresh {
    pub question_id: QuestionId,
}

/// The asker's choice after an `unsure` routing or an answer (SC-015).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum RouteChoice {
    /// A candidate, or "Ask another agent".
    Agent { agent_id: AgentId },
    /// "Ask Minsu": send the question to the person (status `sent_to_owner`).
    Owner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionRouteChoose {
    pub question_id: QuestionId,
    pub choice: RouteChoice,
}

/// The owner hands a question sent to them to one of their agents (SC-010).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionHandoff {
    pub question_id: QuestionId,
    pub agent_id: AgentId,
}

/// The owner answers a question sent to them in person (SC-010).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionAnswer {
    pub question_id: QuestionId,
    pub text: String,
}

/// Deletes this Mac's copy (FR-013). Whether an owner's delete also reaches the
/// asker's copy is still to be confirmed (backend architecture, "Storage and data
/// ownership").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuestionDelete {
    pub question_id: QuestionId,
}

commands! {
    Ask => "ask" -> Question;
    QuestionCancel => "question_cancel" -> Question;
    QuestionRefresh => "question_refresh" -> Question;
    QuestionRouteChoose => "question_route_choose" -> Question;
    QuestionHandoff => "question_handoff" -> Question;
    QuestionAnswer => "question_answer" -> Question;
    QuestionDelete => "question_delete" -> ();
}

/// `question.update`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "change", rename_all = "snake_case")]
#[ts(export)]
pub enum QuestionUpdateEvent {
    Upsert { question: Box<Question> },
    Removed { question_id: QuestionId },
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::testutil::t0;

    pub fn question(asker: MemberId, owner: MemberId, agent_id: AgentId) -> Question {
        let id = QuestionId::generate();
        Question {
            id,
            thread_id: id,
            asker,
            target: AskTarget::Person { member_id: owner },
            text: "Is the login API done?".into(),
            status: QuestionStatus::Answered,
            candidates: vec![],
            agent_id: Some(agent_id),
            answer: Some(Answer {
                id: AnswerId::generate(),
                agent_id: Some(agent_id),
                text: "Yes, merged this morning.".into(),
                excerpts: vec![Excerpt {
                    role: ExcerptRole::Assistant,
                    text: "Merged the login endpoint.".into(),
                    at: t0(),
                }],
                as_of: t0(),
                possibly_stale: false,
                answered_at: t0(),
            }),
            error: None,
            created_at: t0(),
            updated_at: t0(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::question;
    use super::*;
    use crate::common::ErrorCode;
    use crate::testutil::roundtrip;

    #[test]
    fn questions_round_trip() {
        let (asker, owner, agent) = (
            MemberId::generate(),
            MemberId::generate(),
            AgentId::generate(),
        );
        let json = roundtrip(&QuestionUpdateEvent::Upsert {
            question: Box::new(question(asker, owner, agent)),
        });
        assert_eq!(json["change"], "upsert");
        assert_eq!(json["question"]["target"]["kind"], "person");
        assert_eq!(
            json["question"]["answer"]["excerpts"][0]["role"],
            "assistant"
        );

        let mut unsure = question(asker, owner, agent);
        unsure.status = QuestionStatus::Unsure;
        unsure.answer = None;
        unsure.agent_id = None;
        unsure.error = Some(ApiError::new(ErrorCode::PeerOffline, "queued"));
        unsure.candidates = vec![RoutingCandidate {
            agent_id: agent,
            name: "API".into(),
            role: "API developer".into(),
            score: 0.5,
            reason: "Worked on login".into(),
        }];
        assert_eq!(roundtrip(&unsure)["status"], "unsure");
    }

    #[test]
    fn commands_round_trip() {
        let member_id = MemberId::generate();
        roundtrip(&Ask {
            target: AskTarget::Person { member_id },
            text: "Status?".into(),
            thread_id: None,
            from_agent: Some(AgentId::generate()),
        });
        let json = roundtrip(&QuestionRouteChoose {
            question_id: QuestionId::generate(),
            choice: RouteChoice::Owner,
        });
        assert_eq!(json["choice"]["kind"], "owner");
        roundtrip(&QuestionHandoff {
            question_id: QuestionId::generate(),
            agent_id: AgentId::generate(),
        });
        roundtrip(&QuestionUpdateEvent::Removed {
            question_id: QuestionId::generate(),
        });
        roundtrip(&QuestionAnswer {
            question_id: QuestionId::generate(),
            text: "Done".into(),
        });
    }
}
