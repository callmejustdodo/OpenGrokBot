//! The team's local-socket commands, as the service routes them.

use async_trait::async_trait;

use formation_proto::common::{ApiError, Command, ErrorCode};
use formation_proto::policy::Caller;
use formation_proto::team::{TeamCreate, TeamInvite, TeamJoin, TeamRemove};

use super::service::TeamService;
use crate::ports::{CommandCall, CommandHandler, Result, TeamRepo};

/// `team_leave` is not served yet: leaving needs a peer route to the creator's Mac
/// (SC-018, TASK-035).
#[async_trait]
impl<S: TeamRepo + Send + Sync + ?Sized + 'static> CommandHandler for TeamService<S> {
    fn methods(&self) -> Vec<&'static str> {
        vec![
            TeamCreate::METHOD,
            TeamInvite::METHOD,
            TeamJoin::METHOD,
            TeamRemove::METHOD,
        ]
    }

    async fn handle(&self, call: CommandCall) -> Result<serde_json::Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "team commands come only from this Mac's owner",
            ));
        }
        match call.method.as_str() {
            TeamCreate::METHOD => {
                let params: TeamCreate = params(call.params)?;
                output::<TeamCreate>(self.create(params.display_name.as_deref())?)
            }
            TeamInvite::METHOD => {
                let _: TeamInvite = params(call.params)?;
                output::<TeamInvite>(self.invite()?)
            }
            TeamJoin::METHOD => {
                let params: TeamJoin = params(call.params)?;
                output::<TeamJoin>(self.join(&params).await?)
            }
            TeamRemove::METHOD => {
                let params: TeamRemove = params(call.params)?;
                output::<TeamRemove>(self.remove(params.member_id)?)
            }
            other => Err(ApiError::new(
                ErrorCode::Internal,
                format!("the team does not serve `{other}`"),
            )),
        }
    }
}

/// Omitted or `null` params read as `{}`, as `RpcRequest::parse_params` does.
fn params<C: Command>(params: serde_json::Value) -> Result<C> {
    let params = match params {
        serde_json::Value::Null => serde_json::json!({}),
        params => params,
    };
    serde_json::from_value(params)
        .map_err(|error| ApiError::new(ErrorCode::InvalidParams, error.to_string()))
}

fn output<C: Command>(output: C::Output) -> Result<serde_json::Value> {
    serde_json::to_value(output)
        .map_err(|error| ApiError::new(ErrorCode::Internal, error.to_string()))
}
