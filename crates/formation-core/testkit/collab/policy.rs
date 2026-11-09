//! Owner: B. `FakePolicy`: membership, sharing, and tool-call verdicts from a table.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;

use formation_proto::ask::AskTarget;
use formation_proto::common::{AgentId, ApiError, ErrorCode, MemberId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{
    AllowReason, Caller, MAX_CHAIN_DEPTH, PolicyVerdict, ToolCall, Visibility,
};
use formation_proto::team::WhoIs;

use crate::ports::{Policy, Result, ToolContext};

use super::{error, is_read, lock};

/// One call to the fake, with its arguments.
#[derive(Debug, Clone, PartialEq)]
pub enum PolicyCall {
    Authenticate(WhoIs),
    VisibleAgents(Caller),
    CheckAsk {
        caller: Caller,
        target: AskTarget,
    },
    CheckRequest {
        caller: Caller,
        agent_id: AgentId,
    },
    Visibility(AgentId),
    Evaluate {
        context: ToolContext,
        call: ToolCall,
    },
}

/// A [`Policy`] method, for [`FakePolicy::fail_next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyMethod {
    Authenticate,
    VisibleAgents,
    CheckAsk,
    CheckRequest,
    Visibility,
    Evaluate,
}

/// A [`Policy`] that answers from members and shared agents you register.
///
/// Defaults follow the backend architecture's "Where each rule is enforced":
/// unknown Tailscale logins and member IDs get `not_member`; an agent that was
/// not shared is private and gets `agent_not_shared`; a request to an agent that
/// does not accept requests gets `requests_not_accepted`; a chain deeper than
/// [`MAX_CHAIN_DEPTH`] gets `rate_limited`. The per-minute rate limit is not
/// modelled; script it with [`FakePolicy::fail_next`]. The owner (`Caller::Owner`)
/// passes the membership and chain checks; the sharing checks still apply.
///
/// [`Policy::evaluate`] answers, in order: the next verdict pushed with
/// [`FakePolicy::push_verdict`], the verdict set for the tool, the default verdict,
/// and otherwise allow for [`READ_TOOLS`](super::READ_TOOLS) and `needs_approval`
/// for everything else.
#[derive(Debug, Default)]
pub struct FakePolicy {
    state: Mutex<PolicyState>,
}

#[derive(Debug, Default)]
struct PolicyState {
    members: HashMap<String, MemberId>,
    shared: Vec<(SharedAgentView, Visibility)>,
    failures: HashMap<PolicyMethod, VecDeque<ApiError>>,
    verdicts: VecDeque<PolicyVerdict>,
    tool_verdicts: HashMap<String, PolicyVerdict>,
    default_verdict: Option<PolicyVerdict>,
    calls: Vec<PolicyCall>,
}

impl FakePolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a member by the Tailscale login their WhoIs carries.
    pub fn add_member(&self, tailscale_login: impl Into<String>, member_id: MemberId) {
        lock(&self.state)
            .members
            .insert(tailscale_login.into(), member_id);
    }

    /// Ends the member's access at once.
    pub fn remove_member(&self, member_id: MemberId) {
        lock(&self.state).members.retain(|_, id| *id != member_id);
    }

    /// Shares an agent (or updates its view and visibility).
    pub fn share(&self, view: SharedAgentView, visibility: Visibility) {
        let mut state = lock(&self.state);
        state.shared.retain(|(v, _)| v.agent_id != view.agent_id);
        state.shared.push((view, visibility));
    }

    /// Makes the agent private again.
    pub fn unshare(&self, agent_id: AgentId) {
        lock(&self.state)
            .shared
            .retain(|(v, _)| v.agent_id != agent_id);
    }

    /// The next call of `method` fails with `error`. Failures queue up.
    pub fn fail_next(&self, method: PolicyMethod, error: ApiError) {
        lock(&self.state)
            .failures
            .entry(method)
            .or_default()
            .push_back(error);
    }

    /// The next [`Policy::evaluate`] returns `verdict`. Verdicts queue up.
    pub fn push_verdict(&self, verdict: PolicyVerdict) {
        lock(&self.state).verdicts.push_back(verdict);
    }

    pub fn set_tool_verdict(&self, tool_name: impl Into<String>, verdict: PolicyVerdict) {
        lock(&self.state)
            .tool_verdicts
            .insert(tool_name.into(), verdict);
    }

    /// Replaces the built-in "reads allowed, the rest needs approval" default.
    pub fn set_default_verdict(&self, verdict: PolicyVerdict) {
        lock(&self.state).default_verdict = Some(verdict);
    }

    pub fn calls(&self) -> Vec<PolicyCall> {
        lock(&self.state).calls.clone()
    }

    fn begin(
        &self,
        method: PolicyMethod,
        call: PolicyCall,
    ) -> Result<std::sync::MutexGuard<'_, PolicyState>> {
        let mut state = lock(&self.state);
        state.calls.push(call);
        if let Some(error) = state
            .failures
            .get_mut(&method)
            .and_then(VecDeque::pop_front)
        {
            return Err(error);
        }
        Ok(state)
    }
}

impl PolicyState {
    fn check_caller(&self, caller: &Caller) -> Result<()> {
        match caller {
            Caller::Owner => Ok(()),
            Caller::Member {
                member_id, chain, ..
            } => {
                if !self.members.values().any(|id| id == member_id) {
                    return Err(error(
                        ErrorCode::NotMember,
                        format!("{member_id} is not a member"),
                    ));
                }
                if chain.depth > MAX_CHAIN_DEPTH {
                    return Err(error(
                        ErrorCode::RateLimited,
                        format!("chain depth {} is over {MAX_CHAIN_DEPTH}", chain.depth),
                    ));
                }
                Ok(())
            }
        }
    }

    fn shared(&self, agent_id: AgentId) -> Result<&(SharedAgentView, Visibility)> {
        self.shared
            .iter()
            .find(|(view, _)| view.agent_id == agent_id)
            .ok_or_else(|| {
                error(
                    ErrorCode::AgentNotShared,
                    format!("agent {agent_id} is not shared"),
                )
            })
    }
}

#[async_trait]
impl Policy for FakePolicy {
    async fn authenticate(&self, whois: &WhoIs) -> Result<MemberId> {
        let state = self.begin(
            PolicyMethod::Authenticate,
            PolicyCall::Authenticate(whois.clone()),
        )?;
        state.members.get(&whois.login).copied().ok_or_else(|| {
            error(
                ErrorCode::NotMember,
                format!("{} is not a member", whois.login),
            )
        })
    }

    async fn visible_agents(&self, caller: &Caller) -> Result<Vec<SharedAgentView>> {
        let state = self.begin(
            PolicyMethod::VisibleAgents,
            PolicyCall::VisibleAgents(caller.clone()),
        )?;
        state.check_caller(caller)?;
        Ok(state.shared.iter().map(|(view, _)| view.clone()).collect())
    }

    async fn check_ask(&self, caller: &Caller, target: &AskTarget) -> Result<()> {
        let state = self.begin(
            PolicyMethod::CheckAsk,
            PolicyCall::CheckAsk {
                caller: caller.clone(),
                target: *target,
            },
        )?;
        state.check_caller(caller)?;
        if let AskTarget::Agent { agent_id, .. } = target {
            state.shared(*agent_id)?;
        }
        Ok(())
    }

    async fn check_request(&self, caller: &Caller, agent_id: AgentId) -> Result<()> {
        let state = self.begin(
            PolicyMethod::CheckRequest,
            PolicyCall::CheckRequest {
                caller: caller.clone(),
                agent_id,
            },
        )?;
        state.check_caller(caller)?;
        let (view, _) = state.shared(agent_id)?;
        if !view.accepts_requests {
            return Err(error(
                ErrorCode::RequestsNotAccepted,
                format!("agent {agent_id} does not accept requests"),
            ));
        }
        Ok(())
    }

    /// A private agent reports the default visibility of
    /// [`ShareSettings::private`](formation_proto::policy::ShareSettings::private).
    async fn visibility(&self, agent_id: AgentId) -> Result<Visibility> {
        let state = self.begin(PolicyMethod::Visibility, PolicyCall::Visibility(agent_id))?;
        Ok(state
            .shared(agent_id)
            .map(|(_, visibility)| *visibility)
            .unwrap_or(Visibility::AnswerExcerpts))
    }

    async fn evaluate(&self, context: &ToolContext, call: &ToolCall) -> Result<PolicyVerdict> {
        let mut state = self.begin(
            PolicyMethod::Evaluate,
            PolicyCall::Evaluate {
                context: context.clone(),
                call: call.clone(),
            },
        )?;
        if let Some(verdict) = state.verdicts.pop_front() {
            return Ok(verdict);
        }
        if let Some(verdict) = state.tool_verdicts.get(&call.tool_name) {
            return Ok(verdict.clone());
        }
        if let Some(verdict) = &state.default_verdict {
            return Ok(verdict.clone());
        }
        Ok(if is_read(&call.tool_name) {
            PolicyVerdict::Allow {
                reason: AllowReason::AlwaysAllowedRead,
            }
        } else {
            PolicyVerdict::NeedsApproval
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use formation_proto::common::{ChainId, RuleId};
    use formation_proto::policy::Chain;
    use formation_proto::team::MemberRole;

    use super::super::block_on;
    use super::super::samples::{bash, member, member_caller, shared_agent, tool_call, whois_of};
    use super::*;

    fn context(caller: Caller) -> ToolContext {
        ToolContext {
            agent_id: AgentId::generate(),
            folder: PathBuf::from("/Users/dohyeon/Agents/api"),
            repositories: vec![],
            caller,
            request_id: None,
        }
    }

    #[test]
    fn authenticates_members_and_rejects_strangers_and_removed_members() {
        let policy = FakePolicy::new();
        let minsu = member("minsu", MemberRole::Member);
        policy.add_member(&minsu.tailscale_login, minsu.id);
        let port: Arc<dyn Policy> = Arc::new(policy);
        block_on(async {
            assert_eq!(
                port.authenticate(&whois_of(&minsu)).await.unwrap(),
                minsu.id
            );
            let stranger = WhoIs {
                login: "eve@github".into(),
                node: "eve-mbp".into(),
            };
            let error = port.authenticate(&stranger).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember);
        });
    }

    #[test]
    fn private_agents_stay_invisible_and_unaskable() {
        let policy = FakePolicy::new();
        let (me, minsu) = (MemberId::generate(), member("minsu", MemberRole::Member));
        policy.add_member(&minsu.tailscale_login, minsu.id);
        let shared = shared_agent(me, "api", false);
        let private = shared_agent(me, "notes", true);
        policy.share(shared.clone(), Visibility::Answer);
        let caller = member_caller(minsu.id);
        block_on(async {
            let agents = policy.visible_agents(&caller).await.unwrap();
            assert_eq!(agents, vec![shared.clone()]);

            let ask_shared = AskTarget::Agent {
                member_id: me,
                agent_id: shared.agent_id,
            };
            policy.check_ask(&caller, &ask_shared).await.unwrap();
            let ask_private = AskTarget::Agent {
                member_id: me,
                agent_id: private.agent_id,
            };
            let error = policy.check_ask(&caller, &ask_private).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::AgentNotShared);
            let ask_person = AskTarget::Person { member_id: me };
            policy.check_ask(&caller, &ask_person).await.unwrap();

            assert_eq!(
                policy.visibility(shared.agent_id).await.unwrap(),
                Visibility::Answer
            );
            assert_eq!(
                policy.visibility(private.agent_id).await.unwrap(),
                Visibility::AnswerExcerpts
            );

            let stranger = member_caller(MemberId::generate());
            let error = policy.visible_agents(&stranger).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember);

            policy.remove_member(minsu.id);
            let error = policy.check_ask(&caller, &ask_shared).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember);
        });
    }

    #[test]
    fn requests_need_sharing_and_acceptance_and_chains_are_limited() {
        let policy = FakePolicy::new();
        let (me, minsu) = (MemberId::generate(), MemberId::generate());
        policy.add_member("minsu@github", minsu);
        let accepting = shared_agent(me, "api", true);
        let closed = shared_agent(me, "web", false);
        policy.share(accepting.clone(), Visibility::AnswerExcerpts);
        policy.share(closed.clone(), Visibility::AnswerExcerpts);
        block_on(async {
            let caller = member_caller(minsu);
            policy
                .check_request(&caller, accepting.agent_id)
                .await
                .unwrap();
            let error = policy
                .check_request(&caller, closed.agent_id)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RequestsNotAccepted);
            policy.unshare(accepting.agent_id);
            let error = policy
                .check_request(&caller, accepting.agent_id)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::AgentNotShared);

            let deep = Caller::Member {
                member_id: minsu,
                via_agent: Some("web".into()),
                chain: Chain {
                    id: ChainId::generate(),
                    depth: MAX_CHAIN_DEPTH + 1,
                },
            };
            let error = policy
                .check_request(&deep, closed.agent_id)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RateLimited);
            let error = policy
                .check_request(&Caller::Owner, closed.agent_id)
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::RequestsNotAccepted);
        });
    }

    #[test]
    fn evaluate_defaults_to_reads_allowed_and_follows_scripts() {
        let policy = FakePolicy::new();
        let ctx = context(Caller::Owner);
        block_on(async {
            let read = tool_call("Read", serde_json::json!({"file_path": "README.md"}));
            assert_eq!(
                policy.evaluate(&ctx, &read).await.unwrap(),
                PolicyVerdict::Allow {
                    reason: AllowReason::AlwaysAllowedRead
                }
            );
            assert_eq!(
                policy.evaluate(&ctx, &bash("npm test")).await.unwrap(),
                PolicyVerdict::NeedsApproval
            );

            let rule_id = RuleId::generate();
            policy.set_tool_verdict(
                "Bash",
                PolicyVerdict::Allow {
                    reason: AllowReason::Rule { rule_id },
                },
            );
            policy.push_verdict(PolicyVerdict::Deny {
                reason: "command chaining".into(),
            });
            assert!(matches!(
                policy
                    .evaluate(&ctx, &bash("npm test && rm -rf /"))
                    .await
                    .unwrap(),
                PolicyVerdict::Deny { .. }
            ));
            assert_eq!(
                policy.evaluate(&ctx, &bash("npm test")).await.unwrap(),
                PolicyVerdict::Allow {
                    reason: AllowReason::Rule { rule_id }
                }
            );

            policy.set_default_verdict(PolicyVerdict::Deny {
                reason: "locked".into(),
            });
            let write = tool_call("Write", serde_json::json!({"file_path": "a.txt"}));
            assert!(matches!(
                policy.evaluate(&ctx, &write).await.unwrap(),
                PolicyVerdict::Deny { .. }
            ));
        });
    }

    #[test]
    fn scripted_failures_and_recorded_calls() {
        let policy = FakePolicy::new();
        policy.add_member("minsu@github", MemberId::generate());
        policy.fail_next(
            PolicyMethod::CheckAsk,
            ApiError::new(ErrorCode::RateLimited, "31 calls this minute"),
        );
        let target = AskTarget::Person {
            member_id: MemberId::generate(),
        };
        block_on(async {
            let error = policy.check_ask(&Caller::Owner, &target).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::RateLimited);
            policy.check_ask(&Caller::Owner, &target).await.unwrap();
        });
        assert_eq!(
            policy.calls(),
            vec![
                PolicyCall::CheckAsk {
                    caller: Caller::Owner,
                    target
                };
                2
            ]
        );
    }
}
