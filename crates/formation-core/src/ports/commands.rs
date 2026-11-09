//! Owner: A (service). How the local socket reaches each role's command handling
//! without importing its internals. Each role registers one handler for the methods
//! its proto files define; the service routes every method to exactly one handler.

use async_trait::async_trait;

use formation_proto::policy::Caller;

use super::Result;

/// One decoded JSON-RPC request from the local socket.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandCall {
    /// `Caller::Owner` for the local socket; an agent's own calls name it in params
    /// (`from_run`).
    pub caller: Caller,
    pub method: String,
    pub params: serde_json::Value,
}

#[async_trait]
pub trait CommandHandler: Send + Sync {
    /// The methods this handler serves, from the `METHODS` of its proto files.
    fn methods(&self) -> Vec<&'static str>;

    /// Handles one call. Params that do not decode return `invalid_params`.
    async fn handle(&self, call: CommandCall) -> Result<serde_json::Value>;
}
