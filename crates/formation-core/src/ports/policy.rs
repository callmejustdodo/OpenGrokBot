//! Owner: B. Membership, sharing, limits, and tool-call rules (backend architecture,
//! "Where each rule is enforced").

use std::path::PathBuf;

use async_trait::async_trait;

use formation_proto::ask::AskTarget;
use formation_proto::common::{AgentId, MemberId, RequestId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{Caller, PolicyVerdict, ToolCall, Visibility};
use formation_proto::team::WhoIs;

use super::Result;

/// What the policy needs to know about the turn a tool call belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolContext {
    pub agent_id: AgentId,
    pub folder: PathBuf,
    /// Repositories assigned to the agent; reads inside them are always allowed (D-028).
    pub repositories: Vec<PathBuf>,
    pub caller: Caller,
    pub request_id: Option<RequestId>,
}

#[async_trait]
pub trait Policy: Send + Sync {
    /// Maps a peer connection to a member. `not_member` for unknown or removed callers.
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId>;

    /// The caller's view of this Mac's shared agents. Never includes private agents.
    async fn visible_agents(&self, caller: &Caller) -> Result<Vec<SharedAgentView>>;

    /// Before accepting a question: membership, sharing, chain depth, and rate
    /// (`not_member`, `agent_not_shared`, `rate_limited`).
    async fn check_ask(&self, caller: &Caller, target: &AskTarget) -> Result<()>;

    /// Before accepting a work request: as `check_ask`, plus `requests_not_accepted`.
    async fn check_request(&self, caller: &Caller, agent_id: AgentId) -> Result<()>;

    async fn visibility(&self, agent_id: AgentId) -> Result<Visibility>;

    /// Decides one tool call from always-allowed reads and saved rules, without waiting.
    async fn evaluate(&self, context: &ToolContext, call: &ToolCall) -> Result<PolicyVerdict>;
}
