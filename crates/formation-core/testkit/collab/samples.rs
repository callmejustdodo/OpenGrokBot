//! Owner: B. Builders for B's proto values with plausible defaults, so tests can
//! change the one field they care about.

use formation_proto::agent::{AgentState, Avatar, HarnessKind};
use formation_proto::common::{AgentId, ChainId, MemberId, RequestId, RunId, SessionId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{Caller, Chain, PermissionPrompt, PermissionSource, ToolCall};
use formation_proto::request::{RequestStatus, WorkRequest};
use formation_proto::team::{Member, MemberRole, WhoIs};

use crate::ports::PermissionRequest;

use super::now;

/// A member whose Tailscale login is `<github_login>@github` on node `<github_login>-mbp`.
pub fn member(github_login: &str, role: MemberRole) -> Member {
    let at = now();
    Member {
        id: MemberId::generate(),
        github_login: github_login.into(),
        display_name: github_login.into(),
        tailscale_login: format!("{github_login}@github"),
        tailscale_node: format!("{github_login}-mbp"),
        role,
        joined_at: at,
        online: true,
        last_seen_at: Some(at),
    }
}

/// The WhoIs answer a member's Mac produces.
pub fn whois_of(member: &Member) -> WhoIs {
    WhoIs {
        login: member.tailscale_login.clone(),
        node: member.tailscale_node.clone(),
    }
}

/// A teammate (or an agent of theirs) starting a new chain at depth 0.
pub fn member_caller(member_id: MemberId) -> Caller {
    Caller::Member {
        member_id,
        via_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
    }
}

/// An idle Claude Code agent shared by `owner`.
pub fn shared_agent(owner: MemberId, slug: &str, accepts_requests: bool) -> SharedAgentView {
    SharedAgentView {
        owner,
        agent_id: AgentId::generate(),
        slug: slug.into(),
        name: slug.into(),
        role: format!("{slug} agent"),
        harness: HarnessKind::ClaudeCode,
        avatar: Avatar {
            shape: "square".into(),
            color: "green".into(),
        },
        state: AgentState::Idle,
        last_activity_at: None,
        accepts_requests,
    }
}

/// A request just received on the owner's Mac.
pub fn work_request(
    requester: MemberId,
    owner: MemberId,
    agent_id: AgentId,
    prompt: &str,
    idempotency_key: &str,
) -> WorkRequest {
    let at = now();
    WorkRequest {
        id: RequestId::generate(),
        requester,
        requester_agent: None,
        chain: Chain {
            id: ChainId::generate(),
            depth: 0,
        },
        owner,
        agent_id,
        prompt: prompt.into(),
        idempotency_key: idempotency_key.into(),
        status: RequestStatus::Received,
        result: None,
        error: None,
        created_at: at,
        updated_at: at,
    }
}

pub fn tool_call(tool_name: &str, input: serde_json::Value) -> ToolCall {
    ToolCall {
        tool_use_id: Some(format!("toolu_{}", RunId::generate().0.simple())),
        tool_name: tool_name.into(),
        input,
    }
}

/// A `Bash` call running `command`.
pub fn bash(command: &str) -> ToolCall {
    tool_call("Bash", serde_json::json!({ "command": command }))
}

/// A prompt from the permission tool for `call` in run `run_id`.
pub fn permission_request(
    agent_id: AgentId,
    run_id: RunId,
    caller: Caller,
    request_id: Option<RequestId>,
    call: ToolCall,
) -> PermissionRequest {
    PermissionRequest {
        prompt: PermissionPrompt {
            agent_id,
            run_id,
            session_id: Some(SessionId(format!("session-{}", run_id.0.simple()))),
            source: PermissionSource::PermissionTool,
            call,
        },
        caller,
        request_id,
    }
}

/// The same prompt as it arrives through the `PreToolUse` hook.
pub fn from_hook(mut request: PermissionRequest) -> PermissionRequest {
    request.prompt.source = PermissionSource::PreToolUseHook;
    request
}
