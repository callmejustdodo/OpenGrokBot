//! Owner: A (agent runtime).
//!
//! Conversations with an agent's main session and request forks: items, streaming
//! deltas, the message queue, and session facts.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{
    AgentId, ApprovalId, ItemId, MemberId, MessageId, RequestId, ScheduleId, SessionId, Timestamp,
    commands,
};
use crate::request::QueueItem;

/// Who wrote a user message in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Author {
    Owner,
    /// A teammate's work request in that request's fork conversation, possibly sent by their agent.
    Teammate {
        member_id: MemberId,
        via_agent: Option<String>,
        request_id: RequestId,
    },
    Schedule {
        schedule_id: ScheduleId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StepKind {
    /// Edit, Write, MultiEdit.
    Edit,
    /// Bash.
    Command,
    /// Read, Glob, Grep.
    Read,
    /// WebFetch, WebSearch.
    Web,
    /// Any other tool.
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StepStatus {
    Running,
    Done,
    Failed,
    Denied,
}

/// One tool use by the agent, as the conversation shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Step {
    pub kind: StepKind,
    pub tool_name: String,
    /// The file, command, or URL the step acts on.
    pub target: String,
    /// Diff, command output, or other detail shown when expanded.
    pub detail: Option<String>,
    pub status: StepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NoticeLevel {
    Info,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ConversationItem {
    UserMessage {
        id: ItemId,
        at: Timestamp,
        author: Author,
        text: String,
    },
    AssistantText {
        id: ItemId,
        at: Timestamp,
        text: String,
    },
    Step {
        id: ItemId,
        at: Timestamp,
        step: Step,
    },
    /// A permission card; the approval itself comes from `approval.*` events.
    Approval {
        id: ItemId,
        at: Timestamp,
        approval_id: ApprovalId,
    },
    /// A teammate's work request card.
    Request {
        id: ItemId,
        at: Timestamp,
        request_id: RequestId,
    },
    Notice {
        id: ItemId,
        at: Timestamp,
        level: NoticeLevel,
        text: String,
    },
}

/// The recent end of an agent's main-session conversation. Older items come from
/// `conversation_page`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Conversation {
    pub agent_id: AgentId,
    pub session_id: Option<SessionId>,
    pub items: Vec<ConversationItem>,
    pub has_more: bool,
    pub queue: Vec<QueueItem>,
    /// The last `agent.stream` sequence number already included in `items`.
    #[ts(type = "number")]
    pub stream_seq: u64,
}

/// Facts about an agent's main session. The ask pipeline forks from here
/// (TASK-022); the store keeps it per agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionInfo {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub transcript_path: PathBuf,
    /// The agent folder the session runs in.
    pub cwd: PathBuf,
    pub started_at: Timestamp,
    pub last_activity_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

/// One change to the open conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum StreamDelta {
    /// A queued message started a turn.
    TurnStarted {
        message_id: MessageId,
    },
    ItemStarted {
        item: ConversationItem,
    },
    /// Partial assistant text for an item already started. A client drops deltas for
    /// an item it does not have until that item's `ItemCompleted` arrives.
    TextDelta {
        item_id: ItemId,
        text: String,
    },
    /// The final form of an item; replaces what deltas built.
    ItemCompleted {
        item: ConversationItem,
    },
    TurnEnded {
        message_id: MessageId,
        outcome: TurnOutcome,
    },
}

/// `agent.stream`. `stream_seq` counts per agent; a gap means the client reloads the
/// conversation with `conversation_page`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentStreamEvent {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    #[ts(type = "number")]
    pub stream_seq: u64,
    pub delta: StreamDelta,
}

/// `agent.queue`: the whole queue after any change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentQueueEvent {
    pub agent_id: AgentId,
    pub items: Vec<QueueItem>,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConversationPage {
    pub agent_id: AgentId,
    /// `None` selects the main session.
    pub session_id: Option<SessionId>,
    /// Items before this one; the newest items when `None`.
    pub before: Option<ItemId>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ConversationPageResult {
    pub items: Vec<ConversationItem>,
    pub has_more: bool,
    /// The last `agent.stream` sequence number included; after a reload, apply only
    /// later deltas.
    #[ts(type = "number")]
    pub stream_seq: u64,
}

/// Queue a message for the agent's next turn (or start one when idle). Detected
/// agents (sessions started outside Formation) are read and fork only (#14); returns
/// `harness_unsupported` for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentMessageSend {
    pub agent_id: AgentId,
    pub text: String,
}

/// Deliver a queued message into the running turn. It takes effect at the next tool
/// boundary: the running tool finishes and the turn follows the new message (TASK-004).
/// Only the owner's own messages can steer (`denied` otherwise), because the steered
/// text runs under the running turn's caller. `harness_unsupported` for a harness
/// that cannot steer or for a detected agent, which is read and fork only (#14).
/// Returns `conflict` when no turn is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentMessageSteer {
    pub agent_id: AgentId,
    pub message_id: MessageId,
}

/// Edits a queued message that has not started; `conflict` once it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentMessageEdit {
    pub agent_id: AgentId,
    pub message_id: MessageId,
    pub text: String,
}

/// Stops the running turn and keeps the session via `Harness::interrupt` (TASK-004
/// 4a: stdin `control_request` interrupt); `conflict` when no turn runs. Unlike
/// `agent_stop`, this does not stop the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentTurnInterrupt {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentMessageWithdraw {
    pub agent_id: AgentId,
    pub message_id: MessageId,
}

commands! {
    ConversationPage => "conversation_page" -> ConversationPageResult;
    AgentMessageSend => "agent_message_send" -> QueueItem;
    AgentMessageSteer => "agent_message_steer" -> ();
    AgentMessageEdit => "agent_message_edit" -> QueueItem;
    AgentTurnInterrupt => "agent_turn_interrupt" -> ();
    AgentMessageWithdraw => "agent_message_withdraw" -> ();
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::request::samples::queue_item;
    use crate::testutil::t0;

    pub fn conversation(agent_id: AgentId) -> Conversation {
        Conversation {
            agent_id,
            session_id: Some(SessionId("5f0c6c1e-0000-7000-8000-000000000001".into())),
            items: vec![
                ConversationItem::UserMessage {
                    id: ItemId("u1".into()),
                    at: t0(),
                    author: Author::Owner,
                    text: "Run the tests".into(),
                },
                ConversationItem::Step {
                    id: ItemId("s1".into()),
                    at: t0(),
                    step: Step {
                        kind: StepKind::Command,
                        tool_name: "Bash".into(),
                        target: "cargo test".into(),
                        detail: None,
                        status: StepStatus::Done,
                    },
                },
                ConversationItem::AssistantText {
                    id: ItemId("a1".into()),
                    at: t0(),
                    text: "All tests pass.".into(),
                },
            ],
            has_more: false,
            queue: vec![queue_item(agent_id)],
            stream_seq: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::conversation;
    use super::*;
    use crate::testutil::{roundtrip, t0};

    #[test]
    fn conversation_round_trips() {
        let agent_id = AgentId::generate();
        let json = roundtrip(&conversation(agent_id));
        assert_eq!(json["items"][0]["kind"], "user_message");
        assert_eq!(json["items"][0]["author"]["kind"], "owner");
        assert_eq!(json["items"][1]["step"]["kind"], "command");
    }

    #[test]
    fn stream_events_round_trip() {
        let agent_id = AgentId::generate();
        let message_id = MessageId::generate();
        let json = roundtrip(&AgentStreamEvent {
            agent_id,
            session_id: SessionId("fork-session".into()),
            stream_seq: 4,
            delta: StreamDelta::TextDelta {
                item_id: ItemId("a2".into()),
                text: "Look".into(),
            },
        });
        assert_eq!(json["session_id"], "fork-session");
        assert_eq!(json["delta"]["kind"], "text_delta");
        roundtrip(&StreamDelta::TurnEnded {
            message_id,
            outcome: TurnOutcome::Interrupted,
        });
        roundtrip(&SessionInfo {
            agent_id,
            session_id: SessionId("s".into()),
            transcript_path: "/Users/dohyeon/.claude/projects/x/s.jsonl".into(),
            cwd: "/Users/dohyeon/Agents/api".into(),
            started_at: t0(),
            last_activity_at: None,
        });
        roundtrip(&AgentMessageEdit {
            agent_id,
            message_id,
            text: "Updated".into(),
        });
        roundtrip(&AgentTurnInterrupt { agent_id });
        roundtrip(&AgentMessageSteer {
            agent_id,
            message_id,
        });
    }
}
