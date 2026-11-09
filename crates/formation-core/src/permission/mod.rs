//! Owner: B. The permission gate (TASK-044, FR-015, FR-022, NFR-004): every
//! Claude Code tool call reaches it twice, first through the required
//! `PreToolUse` hook and then, when Claude Code's own permission flow asks,
//! through the `--permission-prompt-tool` (`formation mcp --permission`). Both
//! send `permission_decide` over the local socket.
//!
//! - The run named by `FORMATION_RUN_ID` gives the caller: the owner for the
//!   main session and schedules, the requester for a teammate's request fork.
//!   An unknown run, or one of another agent, is denied.
//! - [`Policy::evaluate`] decides first: always-allowed reads and saved rules
//!   allow, anything else needs the owner.
//! - The hook only ever denies or defers (see [`PermissionVerdict::Defer`]), so
//!   it never skips the permission tool and the owner's cards with it.
//! - The permission tool answers allowed and denied calls at once; a call that
//!   needs the owner raises an approval (`approval.raised`, shown as a card in
//!   the conversation for the owner's own work) and waits ([`Approvals::settle`]).
//! - Deny wins: once one prompt for a tool call (same run and `tool_use_id`) is
//!   denied, every later prompt for it is denied too.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use formation_proto::common::{ApiError, ErrorCode, RunId};
use formation_proto::policy::{
    ApprovalDecide, Caller, PermissionDecide, PermissionPrompt, PermissionSource,
    PermissionVerdict, PolicyVerdict,
};
use formation_proto::request::QueueSource;

use crate::policy::approvals::{ApprovalStore, Approvals};
use crate::ports::{
    CommandCall, CommandHandler, Harness, PermissionGate, PermissionRequest, Policy, Result, Store,
    ToolContext,
};

/// The gate behind `permission_decide`.
pub struct Gate {
    harness: Arc<dyn Harness>,
    store: Arc<dyn Store>,
    requests: Arc<dyn ApprovalStore>,
    policy: Arc<dyn Policy>,
    approvals: Arc<Approvals>,
    /// Tool calls already denied, by run and `tool_use_id`.
    denied: Mutex<HashSet<(RunId, String)>>,
}

impl Gate {
    pub fn new(
        harness: Arc<dyn Harness>,
        store: Arc<dyn Store>,
        requests: Arc<dyn ApprovalStore>,
        policy: Arc<dyn Policy>,
        approvals: Arc<Approvals>,
    ) -> Self {
        Self {
            harness,
            store,
            requests,
            policy,
            approvals,
            denied: Mutex::default(),
        }
    }

    /// `permission_decide`: finds the run's caller, then decides.
    pub async fn decide_prompt(&self, prompt: PermissionPrompt) -> Result<PermissionVerdict> {
        let Some(run) = self.harness.run(prompt.run_id).await else {
            return Ok(self.deny(
                &prompt,
                "Formation does not know the run of this tool call.",
            ));
        };
        if run.agent_id != prompt.agent_id {
            return Ok(self.deny(&prompt, "This tool call names another agent than its run."));
        }
        let (caller, request_id) = match run.item.source {
            QueueSource::Owner | QueueSource::Schedule { .. } => (Caller::Owner, None),
            QueueSource::Request {
                request_id,
                requester,
            } => {
                let Some(request) = self.requests.request(request_id)? else {
                    return Ok(self.deny(&prompt, "The work request of this run is gone."));
                };
                if request.requester != requester || request.agent_id != prompt.agent_id {
                    return Ok(self.deny(&prompt, "The work request does not match its run."));
                }
                (
                    Caller::Member {
                        member_id: requester,
                        via_agent: request.requester_agent,
                        chain: request.chain,
                    },
                    Some(request_id),
                )
            }
        };
        self.decide(PermissionRequest {
            prompt,
            caller,
            request_id,
        })
        .await
    }

    fn key(prompt: &PermissionPrompt) -> Option<(RunId, String)> {
        prompt
            .call
            .tool_use_id
            .clone()
            .map(|id| (prompt.run_id, id))
    }

    fn denied(&self) -> std::sync::MutexGuard<'_, HashSet<(RunId, String)>> {
        self.denied.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn remember(&self, prompt: &PermissionPrompt, verdict: &PermissionVerdict) {
        if matches!(verdict, PermissionVerdict::Deny { .. })
            && let Some(key) = Self::key(prompt)
        {
            self.denied().insert(key);
        }
    }

    fn deny(&self, prompt: &PermissionPrompt, message: &str) -> PermissionVerdict {
        let verdict = PermissionVerdict::Deny {
            message: message.into(),
        };
        self.remember(prompt, &verdict);
        verdict
    }
}

#[async_trait]
impl PermissionGate for Gate {
    async fn decide(&self, request: PermissionRequest) -> Result<PermissionVerdict> {
        let prompt = &request.prompt;
        if Self::key(prompt).is_some_and(|key| self.denied().contains(&key)) {
            return Ok(PermissionVerdict::Deny {
                message: "This tool call was already denied.".into(),
            });
        }
        let Some(agent) = self.store.agent(prompt.agent_id)? else {
            return Ok(self.deny(prompt, "The agent of this tool call is gone."));
        };
        let context = ToolContext {
            agent_id: agent.id,
            folder: agent.folder_path,
            repositories: agent.repositories,
            caller: request.caller.clone(),
            request_id: request.request_id,
        };
        let verdict = self.policy.evaluate(&context, &prompt.call).await?;
        let answer = match (prompt.source, verdict) {
            (_, PolicyVerdict::Deny { reason }) => PermissionVerdict::Deny { message: reason },
            (PermissionSource::PreToolUseHook, _) => PermissionVerdict::Defer,
            (PermissionSource::PermissionTool, verdict) => {
                self.approvals.settle(verdict, &request).await?
            }
        };
        self.remember(prompt, &answer);
        Ok(answer)
    }
}

/// Serves `permission_decide` (from the permission tool and the hook) and
/// `approval_decide` (the owner's answer) on the local socket.
pub struct PermissionCommands {
    gate: Arc<Gate>,
    approvals: Arc<Approvals>,
}

impl PermissionCommands {
    pub fn new(gate: Arc<Gate>, approvals: Arc<Approvals>) -> Self {
        Self { gate, approvals }
    }
}

#[async_trait]
impl CommandHandler for PermissionCommands {
    fn methods(&self) -> Vec<&'static str> {
        vec!["permission_decide", "approval_decide"]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "permissions are decided on this Mac only",
            ));
        }
        match call.method.as_str() {
            "permission_decide" => {
                let args: PermissionDecide = decode(call.params)?;
                encode(self.gate.decide_prompt(args.prompt).await?)
            }
            "approval_decide" => {
                let args: ApprovalDecide = decode(call.params)?;
                encode(self.approvals.decide(&call.caller, &args).await?)
            }
            method => Err(ApiError::new(
                ErrorCode::InvalidParams,
                format!("unknown method {method}"),
            )),
        }
    }
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|error| ApiError::new(ErrorCode::InvalidParams, error.to_string()))
}

fn encode<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))
}

#[cfg(test)]
mod tests;
