//! Owner: B (collaboration core).
//!
//! Sharing, callers and chains, approvals, rules, and the permission prompt that
//! Claude Code runs send through `formation mcp --permission` and the `PreToolUse`
//! hook (FR-009, FR-015, FR-019, D-024 to D-028).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{
    AgentId, ApprovalId, ChainId, MemberId, RequestId, RuleId, RunId, SessionId, Timestamp,
    commands,
};
use crate::peer::SharedAgentView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum Visibility {
    /// Teammates see the answer only.
    Answer,
    /// Teammates see the answer and the conversation excerpts it used.
    AnswerExcerpts,
}

/// Sharing settings of one agent. The default is private (D-026).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShareSettings {
    pub agent_id: AgentId,
    pub shared: bool,
    pub visibility: Visibility,
    pub accepts_requests: bool,
}

impl ShareSettings {
    /// Not shared, answers with excerpts once shared, requests off (D-025, D-026, FR-009).
    pub fn private(agent_id: AgentId) -> Self {
        Self {
            agent_id,
            shared: false,
            visibility: Visibility::AnswerExcerpts,
            accepts_requests: false,
        }
    }
}

/// Agent-to-agent chain. A call from a person starts a chain at depth 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Chain {
    pub id: ChainId,
    pub depth: u8,
}

/// Depth over this returns `rate_limited` (FR-019).
pub const MAX_CHAIN_DEPTH: u8 = 3;
/// More calls than this per minute per caller returns `rate_limited` (FR-019).
pub const MAX_CALLS_PER_MINUTE: u32 = 30;

/// Who is asking formationd to do something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum Caller {
    /// This Mac's owner, through the local socket.
    Owner,
    /// A teammate, identified by Tailscale WhoIs, possibly through their agent.
    Member {
        member_id: MemberId,
        via_agent: Option<String>,
        chain: Chain,
    },
}

/// A tool call as Claude Code reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ToolCall {
    pub tool_use_id: Option<String>,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalStatus {
    Pending,
    Allowed,
    Denied,
    /// Unanswered for 24 hours (FR-015).
    Expired,
}

/// The owner's choices on an approval (D-003, SC-011).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ApprovalChoice {
    AllowOnce,
    /// Saves a rule (see [`RuleScope::Command`]).
    KeepAllowingSimilar,
    /// Saves a rule allowing everything for this requester and agent.
    AllowAll,
    Deny,
}

/// What a saved rule allows. Never matches command chaining or a folder it does not name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum RuleScope {
    Command {
        program: String,
        /// Matched exactly as stored; never by text similarity.
        arg_pattern: String,
        folder: PathBuf,
    },
    All,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuleDraft {
    pub scope: RuleScope,
    pub expires_at: Option<Timestamp>,
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Rule {
    pub id: RuleId,
    pub requester: MemberId,
    pub agent_id: AgentId,
    pub scope: RuleScope,
    pub expires_at: Option<Timestamp>,
    pub max_uses: Option<u32>,
    pub uses: u32,
    pub created_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
}

/// One action waiting for the owner. `request_id` is `None` for the owner's own work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Approval {
    pub id: ApprovalId,
    pub agent_id: AgentId,
    pub request_id: Option<RequestId>,
    pub requester: Option<MemberId>,
    pub requester_agent: Option<String>,
    pub purpose: Option<String>,
    pub call: ToolCall,
    /// The exact command or file change, ready to show.
    pub summary: String,
    /// The rule "keep allowing similar" would save.
    pub proposed_rule: Option<RuleDraft>,
    pub status: ApprovalStatus,
    pub choice: Option<ApprovalChoice>,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
    pub expires_at: Timestamp,
}

/// Why a tool call was allowed without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum AllowReason {
    /// Reading inside the agent folder or its assigned repositories (D-028).
    AlwaysAllowedRead,
    Rule {
        rule_id: RuleId,
    },
    Approved {
        approval_id: ApprovalId,
    },
}

/// The policy's answer for one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "verdict", rename_all = "snake_case")]
#[ts(export)]
pub enum PolicyVerdict {
    Allow { reason: AllowReason },
    Deny { reason: String },
    NeedsApproval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionSource {
    /// `--permission-prompt-tool mcp__formation__permission`.
    PermissionTool,
    /// The `PreToolUse` hook injected with `--settings`.
    PreToolUseHook,
}

/// What `formation mcp --permission` and the hook send to formationd. formationd adds
/// the caller from the run identified by `FORMATION_RUN_ID`, passed to the process,
/// permission MCP server, and hook; the permission tool input has no session ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionPrompt {
    pub agent_id: AgentId,
    pub run_id: RunId,
    pub session_id: Option<SessionId>,
    pub source: PermissionSource,
    pub call: ToolCall,
}

/// The gate's answer. `allow` and `deny` mirror the permission tool's reply to Claude
/// Code; the hook command translates them into its own output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "behavior", rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionVerdict {
    Allow {
        /// The tool input to run: the original input unless the gate changed it.
        #[serde(rename = "updatedInput")]
        updated_input: serde_json::Value,
    },
    Deny {
        message: String,
    },
    /// `PreToolUse` hook only, and never for a denied call: make no decision, so
    /// Claude Code's own permission flow and the permission tool decide. A hook that
    /// answered allow would skip that flow, and the owner's permission cards with it.
    Defer,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Fields left `None` keep their value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShareUpdate {
    pub agent_id: AgentId,
    pub shared: Option<bool>,
    pub visibility: Option<Visibility>,
    pub accepts_requests: Option<bool>,
}

/// What teammates would see (SC-008 preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SharePreview {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SharePreviewResult {
    pub view: SharedAgentView,
    pub visibility: Visibility,
}

/// Accepted only from the owner's own local socket, never from peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalDecide {
    pub approval_id: ApprovalId,
    pub choice: ApprovalChoice,
    /// The rule as the owner edited it, for `keep_allowing_similar` and `allow_all`.
    pub rule: Option<RuleDraft>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RulesList {}

/// Replaces both limits; `None` removes that limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RulesUpdate {
    pub rule_id: RuleId,
    pub expires_at: Option<Timestamp>,
    pub max_uses: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RulesRevoke {
    pub rule_id: RuleId,
}

/// Sent by `formation mcp --permission` and the `PreToolUse` hook; waits for a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PermissionDecide {
    pub prompt: PermissionPrompt,
}

commands! {
    ShareUpdate => "share_update" -> ShareSettings;
    SharePreview => "share_preview" -> SharePreviewResult;
    ApprovalDecide => "approval_decide" -> Approval;
    RulesList => "rules_list" -> Vec<Rule>;
    RulesUpdate => "rules_update" -> Rule;
    RulesRevoke => "rules_revoke" -> Rule;
    PermissionDecide => "permission_decide" -> PermissionVerdict;
}

/// `approval.raised` and `approval.update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApprovalEvent {
    pub approval: Approval,
}

/// `share.update`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ShareUpdateEvent {
    pub share: ShareSettings,
}

/// `rule.update`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuleUpdateEvent {
    pub rule: Rule,
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::testutil::t0;
    use serde_json::json;

    pub fn approval(agent_id: AgentId, requester: MemberId) -> Approval {
        Approval {
            id: ApprovalId::generate(),
            agent_id,
            request_id: Some(RequestId::generate()),
            requester: Some(requester),
            requester_agent: Some("web".into()),
            purpose: Some("Check the API contract".into()),
            call: ToolCall {
                tool_use_id: Some("toolu_1".into()),
                tool_name: "Bash".into(),
                input: json!({"command": "npm test"}),
            },
            summary: "npm test".into(),
            proposed_rule: Some(RuleDraft {
                scope: RuleScope::Command {
                    program: "npm".into(),
                    arg_pattern: "test".into(),
                    folder: "/Users/dohyeon/Agents/api".into(),
                },
                expires_at: None,
                max_uses: Some(10),
            }),
            status: ApprovalStatus::Pending,
            choice: None,
            created_at: t0(),
            decided_at: None,
            expires_at: t0(),
        }
    }

    pub fn rule(agent_id: AgentId, requester: MemberId) -> Rule {
        Rule {
            id: RuleId::generate(),
            requester,
            agent_id,
            scope: RuleScope::All,
            expires_at: None,
            max_uses: None,
            uses: 0,
            created_at: t0(),
            revoked_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::*;
    use super::*;
    use crate::testutil::roundtrip;
    use serde_json::json;

    #[test]
    fn sharing_defaults_to_private() {
        let share = ShareSettings::private(AgentId::generate());
        assert!(!share.shared && !share.accepts_requests);
        assert_eq!(roundtrip(&share)["visibility"], "answer_excerpts");
    }

    #[test]
    fn approvals_and_rules_round_trip() {
        let agent_id = AgentId::generate();
        let member = MemberId::generate();
        let json = roundtrip(&ApprovalEvent {
            approval: approval(agent_id, member),
        });
        assert_eq!(json["approval"]["status"], "pending");
        assert_eq!(
            json["approval"]["proposed_rule"]["scope"]["kind"],
            "command"
        );
        assert_eq!(
            roundtrip(&rule(agent_id, member))["scope"],
            json!({"kind": "all"})
        );
        roundtrip(&ApprovalDecide {
            approval_id: ApprovalId::generate(),
            choice: ApprovalChoice::KeepAllowingSimilar,
            rule: None,
        });
        let json = roundtrip(&Caller::Member {
            member_id: member,
            via_agent: Some("web".into()),
            chain: Chain {
                id: ChainId::generate(),
                depth: 1,
            },
        });
        assert_eq!(json["kind"], "member");
        assert_eq!(
            roundtrip(&PolicyVerdict::Allow {
                reason: AllowReason::AlwaysAllowedRead
            })["reason"]["kind"],
            "always_allowed_read"
        );
    }

    #[test]
    fn permission_verdict_matches_the_claude_code_reply() {
        let allow = PermissionVerdict::Allow {
            updated_input: json!({"command": "npm test"}),
        };
        assert_eq!(
            roundtrip(&allow),
            json!({"behavior": "allow", "updatedInput": {"command": "npm test"}})
        );
        let deny = PermissionVerdict::Deny {
            message: "Denied by the owner".into(),
        };
        assert_eq!(roundtrip(&deny)["behavior"], "deny");
        assert_eq!(
            roundtrip(&PermissionVerdict::Defer),
            json!({"behavior": "defer"})
        );
        let run_id = RunId::generate();
        let json = roundtrip(&PermissionDecide {
            prompt: PermissionPrompt {
                agent_id: AgentId::generate(),
                run_id,
                session_id: None,
                source: PermissionSource::PreToolUseHook,
                call: ToolCall {
                    tool_use_id: None,
                    tool_name: "Write".into(),
                    input: json!({"file_path": "a.txt"}),
                },
            },
        });
        assert_eq!(json["prompt"]["run_id"], run_id.0.to_string());
    }
}
