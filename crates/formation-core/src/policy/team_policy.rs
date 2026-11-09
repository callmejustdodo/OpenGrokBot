//! The real [`Policy`] (backend architecture, "Where each rule is enforced"):
//! membership from the roster, sharing from the share settings, chain depth and
//! the per-minute limit (FR-019), and tool calls from the always-allowed reads
//! (D-028).
//!
//! - A peer caller is a member only when the recorded Tailscale login and node
//!   both match a current member ([`team::authenticate`]); this Mac's own member
//!   is refused too, since WhoIs names a machine and another process or OS user
//!   on this Mac would otherwise pass as a teammate (tech spec, "Binding and
//!   callers").
//! - A member caller must still be a current member, its chain no deeper than
//!   [`MAX_CHAIN_DEPTH`], and its calls (asks and requests together) no more than
//!   [`MAX_CALLS_PER_MINUTE`] in any minute. The owner is never limited.
//! - Private agents are never listed, asked, or sent requests (D-026).
//! - Tool calls: always-allowed reads are allowed, everything else needs the
//!   owner. Saved rules (TASK-028) will come between the two.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use formation_proto::ask::AskTarget;
use formation_proto::common::{AgentId, ApiError, ErrorCode, MemberId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{
    Caller, MAX_CALLS_PER_MINUTE, MAX_CHAIN_DEPTH, PolicyVerdict, ToolCall, Visibility,
};
use formation_proto::team::WhoIs;

use crate::policy::{reads, sharing};
use crate::ports::{AgentRepo, CollabStore, Policy, Result, ToolContext};
use crate::team;

const WINDOW: Duration = Duration::from_secs(60);

/// [`Policy`] over the roster and share settings in the collaboration store and
/// the agents in A's store.
pub struct TeamPolicy {
    collab: Arc<dyn CollabStore>,
    agents: Arc<dyn AgentRepo + Send + Sync>,
    /// Recent calls per member, oldest first, within [`WINDOW`].
    calls: Mutex<HashMap<MemberId, VecDeque<Instant>>>,
}

impl TeamPolicy {
    pub fn new(collab: Arc<dyn CollabStore>, agents: Arc<dyn AgentRepo + Send + Sync>) -> Self {
        Self {
            collab,
            agents,
            calls: Mutex::default(),
        }
    }

    /// Membership and chain depth, without counting a call.
    fn check_member(&self, caller: &Caller) -> Result<Option<MemberId>> {
        let Caller::Member {
            member_id, chain, ..
        } = caller
        else {
            return Ok(None);
        };
        // `members` lists current members only; `member` also finds removed ones.
        let current = self
            .collab
            .members()?
            .iter()
            .any(|member| member.id == *member_id);
        if !current || self.collab.me()? == Some(*member_id) {
            return Err(ApiError::new(
                ErrorCode::NotMember,
                format!("{member_id} is not a member of this team"),
            ));
        }
        if chain.depth > MAX_CHAIN_DEPTH {
            return Err(ApiError::new(
                ErrorCode::RateLimited,
                format!("chain depth {} is over {MAX_CHAIN_DEPTH}", chain.depth),
            ));
        }
        Ok(Some(*member_id))
    }

    /// [`check_member`](Self::check_member), then counts one call against the
    /// per-minute limit.
    fn admit(&self, caller: &Caller) -> Result<()> {
        let Some(member_id) = self.check_member(caller)? else {
            return Ok(());
        };
        self.count(member_id, Instant::now())
    }

    fn count(&self, member_id: MemberId, now: Instant) -> Result<()> {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let recent = calls.entry(member_id).or_default();
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= MAX_CALLS_PER_MINUTE as usize {
            return Err(ApiError::new(
                ErrorCode::RateLimited,
                format!("more than {MAX_CALLS_PER_MINUTE} calls in a minute"),
            ));
        }
        recent.push_back(now);
        Ok(())
    }

    fn owner(&self) -> Result<MemberId> {
        self.collab
            .me()?
            .ok_or_else(|| ApiError::new(ErrorCode::NotMember, "this Mac is not in a team"))
    }

    /// The agent, which must exist on this Mac.
    fn require_agent(&self, agent_id: AgentId) -> Result<()> {
        if self.agents.agent(agent_id)?.is_some() {
            Ok(())
        } else {
            Err(ApiError::new(
                ErrorCode::AgentNotShared,
                format!("agent {agent_id} is not shared"),
            ))
        }
    }
}

#[async_trait]
impl Policy for TeamPolicy {
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId> {
        let member = team::authenticate(self.collab.as_ref(), Some(whois))?;
        if self.collab.me()? == Some(member.id) {
            return Err(ApiError::new(
                ErrorCode::NotMember,
                "calls from this Mac's own member are refused",
            ));
        }
        Ok(member.id)
    }

    async fn visible_agents(&self, caller: &Caller) -> Result<Vec<SharedAgentView>> {
        self.check_member(caller)?;
        let owner = self.owner()?;
        Ok(sharing::visible_agents(
            owner,
            &self.agents.agents()?,
            &self.collab.shares()?,
        ))
    }

    async fn check_ask(&self, caller: &Caller, target: &AskTarget) -> Result<()> {
        if let AskTarget::Agent { agent_id, .. } = target {
            self.check_member(caller)?;
            self.require_agent(*agent_id)?;
            sharing::require_shared(&self.collab.share(*agent_id)?)?;
        }
        self.admit(caller)
    }

    async fn check_request(&self, caller: &Caller, agent_id: AgentId) -> Result<()> {
        self.check_member(caller)?;
        self.require_agent(agent_id)?;
        sharing::require_requests(&self.collab.share(agent_id)?)?;
        self.admit(caller)
    }

    async fn visibility(&self, agent_id: AgentId) -> Result<Visibility> {
        Ok(self.collab.share(agent_id)?.visibility)
    }

    async fn evaluate(&self, context: &ToolContext, call: &ToolCall) -> Result<PolicyVerdict> {
        Ok(reads::evaluate(context, call))
    }
}

#[cfg(test)]
mod tests;
