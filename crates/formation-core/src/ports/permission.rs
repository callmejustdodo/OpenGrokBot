//! Owner: B. The gate every Claude Code tool call passes through: the permission
//! tool and the `PreToolUse` hook both end here (TASK-044, NFR-004).

use async_trait::async_trait;

use formation_proto::common::RequestId;
use formation_proto::policy::{Caller, PermissionPrompt, PermissionVerdict};

use super::Result;

/// A prompt from the harness, with the caller of the turn that is running.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub prompt: PermissionPrompt,
    pub caller: Caller,
    pub request_id: Option<RequestId>,
}

#[async_trait]
pub trait PermissionGate: Send + Sync {
    /// Asks the policy; when it needs approval, raises one (`approval.raised`) and
    /// waits for the owner's decision or expiry. Deny wins when the permission tool
    /// and the hook disagree.
    async fn decide(&self, request: PermissionRequest) -> Result<PermissionVerdict>;
}
