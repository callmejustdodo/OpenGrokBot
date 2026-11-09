//! The socket call behind the permission tool and the `PreToolUse` hook.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use formation_core::harness::claude::{AGENT_ID_ENV, RUN_ID_ENV};
use formation_core::service::default_socket_path;
use formation_proto::common::{AgentId, RunId, SessionId};
use formation_proto::policy::{
    PermissionDecide, PermissionPrompt, PermissionSource, PermissionVerdict, ToolCall,
};

pub fn deny(reason: impl std::fmt::Display) -> PermissionVerdict {
    PermissionVerdict::Deny {
        message: format!("Formation denied this tool call: {reason}"),
    }
}

/// Asks formationd; every failure denies.
pub async fn ask(
    source: PermissionSource,
    call: ToolCall,
    session_id: Option<SessionId>,
) -> PermissionVerdict {
    let prompt = match prompt_from_env(source, call, session_id) {
        Ok(prompt) => prompt,
        Err(reason) => return deny(reason),
    };
    match permission_decide(prompt).await {
        Ok(verdict) => verdict,
        Err(reason) => deny(reason),
    }
}

fn id_from_env<T: serde::de::DeserializeOwned>(name: &str) -> Result<T, String> {
    let value = std::env::var(name).map_err(|_| format!("{name} is not set"))?;
    serde_json::from_value(Value::String(value)).map_err(|_| format!("{name} is not an ID"))
}

fn prompt_from_env(
    source: PermissionSource,
    call: ToolCall,
    session_id: Option<SessionId>,
) -> Result<PermissionPrompt, String> {
    Ok(PermissionPrompt {
        agent_id: id_from_env::<AgentId>(AGENT_ID_ENV)?,
        run_id: id_from_env::<RunId>(RUN_ID_ENV)?,
        session_id,
        source,
        call,
    })
}

async fn permission_decide(prompt: PermissionPrompt) -> Result<PermissionVerdict, String> {
    let path = default_socket_path().map_err(|error| format!("no formationd socket: {error}"))?;
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|error| format!("formationd is not reachable: {error}"))?;
    let (read, mut write) = stream.into_split();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "permission_decide",
        "params": PermissionDecide { prompt },
    });
    let mut line = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    line.push(b'\n');
    write
        .write_all(&line)
        .await
        .map_err(|error| format!("cannot reach formationd: {error}"))?;
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| format!("formationd closed the connection: {error}"))?
    {
        let reply: Value =
            serde_json::from_str(&line).map_err(|_| "formationd sent no JSON".to_string())?;
        if reply.get("id") != Some(&json!(1)) {
            continue;
        }
        return decode_reply(reply);
    }
    Err("formationd closed the connection without an answer".into())
}

fn decode_reply(reply: Value) -> Result<PermissionVerdict, String> {
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("formationd refused: {message}"));
    }
    let result = reply
        .get("result")
        .cloned()
        .ok_or("formationd sent no result")?;
    serde_json::from_value(result).map_err(|_| "formationd sent an unknown answer".into())
}

/// The tool call and session from the `PreToolUse` hook input.
pub fn hook_call(payload: &Value) -> (ToolCall, Option<SessionId>) {
    let call = ToolCall {
        tool_use_id: payload["tool_use_id"].as_str().map(String::from),
        tool_name: payload["tool_name"].as_str().unwrap_or_default().into(),
        input: payload.get("tool_input").cloned().unwrap_or(Value::Null),
    };
    let session_id = payload["session_id"]
        .as_str()
        .map(|id| SessionId(id.into()));
    (call, session_id)
}

/// The hook's stdout: a deny decision, or nothing so Claude Code's permission flow
/// (and with it the permission tool) decides. The hook never allows, so it never
/// skips the owner's approval cards.
pub fn hook_output(verdict: &PermissionVerdict) -> Option<Value> {
    match verdict {
        PermissionVerdict::Deny { message } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": message,
            }
        })),
        PermissionVerdict::Allow { .. } | PermissionVerdict::Defer => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_input_maps_to_a_tool_call() {
        let (call, session) = hook_call(&json!({
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "npm test"},
            "tool_use_id": "toolu_1",
        }));
        assert_eq!(call.tool_name, "Bash");
        assert_eq!(call.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(call.input, json!({"command": "npm test"}));
        assert_eq!(session, Some(SessionId("s1".into())));
    }

    #[test]
    fn the_hook_only_ever_denies() {
        assert_eq!(
            hook_output(&deny("no")).unwrap()["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        assert!(hook_output(&PermissionVerdict::Defer).is_none());
        assert!(
            hook_output(&PermissionVerdict::Allow {
                updated_input: json!({})
            })
            .is_none()
        );
    }

    #[test]
    fn replies_decode_and_errors_deny() {
        assert_eq!(
            decode_reply(json!({"jsonrpc": "2.0", "id": 1, "result": {"behavior": "defer"}})),
            Ok(PermissionVerdict::Defer)
        );
        assert!(decode_reply(json!({"id": 1, "error": {"code": -32000, "message": "x"}})).is_err());
        assert!(decode_reply(json!({"id": 1, "result": {"behavior": "maybe"}})).is_err());
    }

    #[tokio::test]
    async fn a_missing_run_denies_without_calling_formationd() {
        // SAFETY: this test is the only one in the binary touching these variables.
        unsafe {
            std::env::remove_var(RUN_ID_ENV);
            std::env::remove_var(AGENT_ID_ENV);
        }
        let verdict = ask(
            PermissionSource::PermissionTool,
            ToolCall {
                tool_use_id: None,
                tool_name: "Bash".into(),
                input: json!({}),
            },
            None,
        )
        .await;
        assert!(matches!(verdict, PermissionVerdict::Deny { .. }));
    }
}
