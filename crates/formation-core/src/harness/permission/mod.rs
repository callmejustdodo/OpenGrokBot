//! TASK-005 permission-routing spike: decision and logging shared by the
//! `permission_spike` example's `mcp --permission` tool and `hook pre-tool-use` hook.
//!
//! This is measuring code, not the permission gate. Decisions come from
//! environment variables so a run can be set up from the outside:
//! - `FORMATION_SPIKE_LOG`: JSON Lines file every call is appended to.
//! - `FORMATION_SPIKE_DECISION`: `allow` allows; anything else, or unset, denies.
//! - `FORMATION_SPIKE_HOOK_DECISION`: `allow`, `deny` or `none` for the hook only
//!   (`none` returns no decision, so Claude Code continues its normal permission
//!   flow); unset means the hook follows `FORMATION_SPIKE_DECISION`.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub const LOG_ENV: &str = "FORMATION_SPIKE_LOG";
pub const DECISION_ENV: &str = "FORMATION_SPIKE_DECISION";
pub const HOOK_DECISION_ENV: &str = "FORMATION_SPIKE_HOOK_DECISION";

const DENY_MESSAGE: &str = "Denied by the Formation permission spike";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    /// Default deny: only the exact value `allow` allows.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("allow") => Decision::Allow,
            _ => Decision::Deny,
        }
    }
}

/// The hook's decision; `None` means "no decision" (exit 0, no output).
pub fn hook_decision(hook_value: Option<&str>, shared_value: Option<&str>) -> Option<Decision> {
    match hook_value {
        Some("none") => None,
        Some(value) => Some(Decision::parse(Some(value))),
        None => Some(Decision::parse(shared_value)),
    }
}

/// Result text of the `--permission-prompt-tool`, in the shape documented for
/// `canUseTool` (`{"behavior":"allow","updatedInput":...}` or
/// `{"behavior":"deny","message":...}`).
pub fn permission_tool_response(decision: Decision, input: &Value) -> Value {
    match decision {
        Decision::Allow => json!({ "behavior": "allow", "updatedInput": input }),
        Decision::Deny => json!({ "behavior": "deny", "message": DENY_MESSAGE }),
    }
}

/// `PreToolUse` hook output (hooks reference, "PreToolUse decision control").
pub fn hook_response(decision: Decision) -> Value {
    let (value, reason) = match decision {
        Decision::Allow => ("allow", "Allowed by the Formation permission spike"),
        Decision::Deny => ("deny", DENY_MESSAGE),
    };
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": value,
            "permissionDecisionReason": reason,
        }
    })
}

/// Appends one JSON line to `FORMATION_SPIKE_LOG`, if set. Logging failures are
/// reported on stderr and do not change the decision.
pub fn log_call(source: &str, tool_name: &str, input: &Value, raw: &Value, answer: &Value) {
    let Ok(path) = std::env::var(LOG_ENV) else {
        return;
    };
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let line = json!({
        "ts_ms": ts_ms,
        "source": source,
        "tool_name": tool_name,
        "input": input,
        "raw": raw,
        "answer": answer,
    });
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(err) = result {
        eprintln!("formation spike: cannot append to {path}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_defaults_to_deny() {
        assert_eq!(Decision::parse(None), Decision::Deny);
        assert_eq!(Decision::parse(Some("")), Decision::Deny);
        assert_eq!(Decision::parse(Some("ALLOW")), Decision::Deny);
        assert_eq!(Decision::parse(Some("yes")), Decision::Deny);
        assert_eq!(Decision::parse(Some("allow")), Decision::Allow);
    }

    #[test]
    fn hook_decision_follows_shared_value_unless_overridden() {
        assert_eq!(hook_decision(None, None), Some(Decision::Deny));
        assert_eq!(hook_decision(None, Some("allow")), Some(Decision::Allow));
        assert_eq!(hook_decision(Some("none"), Some("allow")), None);
        assert_eq!(
            hook_decision(Some("deny"), Some("allow")),
            Some(Decision::Deny)
        );
        assert_eq!(
            hook_decision(Some("allow"), Some("deny")),
            Some(Decision::Allow)
        );
        assert_eq!(hook_decision(Some("bogus"), None), Some(Decision::Deny));
    }

    #[test]
    fn permission_tool_allow_echoes_input() {
        let input = json!({ "command": "echo hi" });
        assert_eq!(
            permission_tool_response(Decision::Allow, &input),
            json!({ "behavior": "allow", "updatedInput": { "command": "echo hi" } })
        );
    }

    #[test]
    fn permission_tool_deny_carries_message() {
        let answer = permission_tool_response(Decision::Deny, &json!({}));
        assert_eq!(answer["behavior"], "deny");
        assert!(answer["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert!(answer.get("updatedInput").is_none());
    }

    #[test]
    fn hook_response_shape() {
        let deny = hook_response(Decision::Deny);
        assert_eq!(deny["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(deny["hookSpecificOutput"]["permissionDecision"], "deny");
        let allow = hook_response(Decision::Allow);
        assert_eq!(allow["hookSpecificOutput"]["permissionDecision"], "allow");
    }
}
