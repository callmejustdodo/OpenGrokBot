use async_trait::async_trait;
use formation_core::policy::reads;
use formation_core::ports::{Policy, Result, ToolContext};
use formation_proto::ask::AskTarget;
use formation_proto::common::{AgentId, ApiError, ErrorCode, MemberId};
use formation_proto::peer::SharedAgentView;
use formation_proto::policy::{Caller, PolicyVerdict, ToolCall, Visibility};
use formation_proto::team::WhoIs;

// TEMPORARY (#107): replaced by B's real policy (rules, sharing, membership).
// Owner-only: reads inside the agent's folders run, every other call of the
// owner's own runs needs the owner, and teammates get nothing.
pub struct OwnerOnly;

fn no_teammates<T>() -> Result<T> {
    Err(ApiError::new(
        ErrorCode::NotMember,
        "this Mac does not serve teammates yet (#107)",
    ))
}

#[async_trait]
impl Policy for OwnerOnly {
    async fn authenticate(&self, _whois: &WhoIs) -> Result<MemberId> {
        no_teammates()
    }
    async fn visible_agents(&self, _caller: &Caller) -> Result<Vec<SharedAgentView>> {
        Ok(vec![])
    }
    async fn check_ask(&self, _caller: &Caller, _target: &AskTarget) -> Result<()> {
        no_teammates()
    }
    async fn check_request(&self, _caller: &Caller, _agent_id: AgentId) -> Result<()> {
        no_teammates()
    }
    async fn visibility(&self, _agent_id: AgentId) -> Result<Visibility> {
        Ok(Visibility::Answer)
    }
    async fn evaluate(&self, context: &ToolContext, call: &ToolCall) -> Result<PolicyVerdict> {
        Ok(match context.caller {
            Caller::Owner => reads::evaluate(context, call),
            Caller::Member { .. } => PolicyVerdict::Deny {
                reason: "Teammates' requests cannot use tools on this Mac yet.".into(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formation_proto::common::ChainId;
    use formation_proto::policy::Chain;
    use serde_json::json;

    fn context(caller: Caller) -> ToolContext {
        ToolContext {
            agent_id: AgentId::generate(),
            folder: std::env::temp_dir(),
            repositories: vec![],
            caller,
            request_id: None,
        }
    }

    #[tokio::test]
    async fn denies_teammates_and_asks_the_owner() {
        let write = ToolCall {
            tool_use_id: Some("t1".into()),
            tool_name: "Bash".into(),
            input: json!({"command": "touch x"}),
        };
        let owner = OwnerOnly.evaluate(&context(Caller::Owner), &write).await;
        assert_eq!(owner.unwrap(), PolicyVerdict::NeedsApproval);
        let teammate = Caller::Member {
            member_id: MemberId::generate(),
            via_agent: None,
            chain: Chain {
                id: ChainId::generate(),
                depth: 0,
            },
        };
        let member = OwnerOnly.evaluate(&context(teammate), &write).await;
        assert!(matches!(member.unwrap(), PolicyVerdict::Deny { .. }));
    }
}
