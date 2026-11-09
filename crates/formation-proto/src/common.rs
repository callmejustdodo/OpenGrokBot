//! Owner: A (agent runtime).
//!
//! IDs, time, error codes, and the JSON-RPC 2.0 envelope used on the local
//! socket between formationd and its clients (the app, the CLI, the MCP server).

use std::fmt;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// A UTC instant. Stored and sent as RFC 3339; shown in local time.
pub type Timestamp = DateTime<Utc>;

macro_rules! uuid_ids {
    ($($(#[$meta:meta])* $name:ident),+ $(,)?) => {$(
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new time-ordered ID (UUIDv7, as the data model requires).
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    )+};
}

uuid_ids! {
    /// A Formation agent, unique on its owner's Mac.
    AgentId,
    /// A team member.
    MemberId,
    InvitationId,
    /// A question. The asker's Mac generates it, so outbox retries stay idempotent.
    QuestionId,
    AnswerId,
    /// A work request. The requester's Mac generates it.
    RequestId,
    ApprovalId,
    RuleId,
    ScheduleId,
    ScheduleRunId,
    /// A message waiting in an agent's queue (owner message, work request, or schedule run).
    MessageId,
    /// A needs-attention item.
    AttentionId,
    ActivityId,
    /// Links agent-to-agent calls so chains can be limited (FR-019).
    ChainId,
}

/// A harness session ID (a Claude Code session UUID, later a Codex thread ID). Opaque to Formation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SessionId(pub String);

/// A conversation item ID taken from the harness transcript. Opaque to Formation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ItemId(pub String);

/// Event sequence number on the local socket. Increases by one per event; a gap means
/// the client missed events and must take a new snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Seq(#[ts(type = "number")] pub u64);

impl Seq {
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Error codes shared by the local socket, the peer protocol, the CLI, and MCP.
/// Source: backend architecture, "Error codes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    NotMember,
    AgentNotShared,
    RequestsNotAccepted,
    NotFound,
    /// The peer is unreachable; the question or request waits in the outbox.
    PeerOffline,
    ApprovalExpired,
    Denied,
    RateLimited,
    /// The harness is not installed or not signed in.
    HarnessUnavailable,
    /// A detected agent cannot do this.
    HarnessUnsupported,
    /// The state changed (for example, a file changed on disk, or a name is taken).
    Conflict,
    /// The params do not match the command (CLI exit code 2, usage).
    InvalidParams,
    /// Also what an older Mac reads for a code it does not know yet.
    #[serde(other)]
    Internal,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 13] = [
        Self::NotMember,
        Self::AgentNotShared,
        Self::RequestsNotAccepted,
        Self::NotFound,
        Self::PeerOffline,
        Self::ApprovalExpired,
        Self::Denied,
        Self::RateLimited,
        Self::HarnessUnavailable,
        Self::HarnessUnsupported,
        Self::Conflict,
        Self::InvalidParams,
        Self::Internal,
    ];

    /// CLI exit code (tech spec, "CLI and MCP server"). 0 ok and 6 timeout are decided
    /// by the CLI itself and have no error code.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::InvalidParams => 2,
            Self::NotMember | Self::AgentNotShared | Self::RequestsNotAccepted | Self::Denied => 3,
            Self::NotFound => 4,
            Self::PeerOffline => 5,
            Self::ApprovalExpired
            | Self::RateLimited
            | Self::HarnessUnavailable
            | Self::HarnessUnsupported
            | Self::Conflict
            | Self::Internal => 1,
        }
    }
}

/// An error as clients see it. The message is for logs and developers; the UI shows
/// localized copy chosen by `code`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 over the local socket (newline-delimited).
// ---------------------------------------------------------------------------

pub const JSONRPC_VERSION: &str = "2.0";

/// Method name of the notification formationd sends for every event; params is an
/// [`EventEnvelope`](crate::events::EventEnvelope).
pub const EVENT_NOTIFICATION: &str = "event";

/// JSON-RPC error code for every Formation error; `data` carries the [`ApiError`].
pub const RPC_APPLICATION_ERROR: i32 = -32000;
pub const RPC_PARSE_ERROR: i32 = -32700;
pub const RPC_INVALID_REQUEST: i32 = -32600;
pub const RPC_METHOD_NOT_FOUND: i32 = -32601;
pub const RPC_INVALID_PARAMS: i32 = -32602;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: RpcId,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl RpcRequest {
    /// A request for a typed command.
    pub fn new<C: Command>(id: RpcId, params: &C) -> serde_json::Result<Self> {
        Ok(Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: C::METHOD.to_owned(),
            params: serde_json::to_value(params)?,
        })
    }

    /// Decodes `params` as command `C`. Omitted or `null` params read as `{}`, so
    /// commands without fields accept both.
    pub fn parse_params<C: Command>(&self) -> serde_json::Result<C> {
        match &self.params {
            serde_json::Value::Null => serde_json::from_value(serde_json::json!({})),
            params => C::deserialize(params),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcResponse {
    Success {
        jsonrpc: String,
        id: RpcId,
        result: serde_json::Value,
    },
    Failure {
        jsonrpc: String,
        id: Option<RpcId>,
        error: RpcError,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default)]
    pub data: Option<ApiError>,
}

impl From<ApiError> for RpcError {
    fn from(error: ApiError) -> Self {
        Self {
            code: RPC_APPLICATION_ERROR,
            message: error.message.clone(),
            data: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: serde_json::Value,
}

/// Binds a command's params type to its JSON-RPC method name and result type.
/// Every command on the local socket implements this, so the name and both shapes
/// live in one place.
pub trait Command: Serialize + DeserializeOwned {
    const METHOD: &'static str;
    type Output: Serialize + DeserializeOwned;
}

/// Implements [`Command`] for each params type and defines `METHODS` for the module.
macro_rules! commands {
    ($($params:ident => $method:literal -> $output:ty;)+) => {
        $(
            impl $crate::common::Command for $params {
                const METHOD: &'static str = $method;
                type Output = $output;
            }
        )+

        /// JSON-RPC method names defined in this module.
        pub const METHODS: &[&str] = &[$($method),+];
    };
}
pub(crate) use commands;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::roundtrip;

    #[test]
    fn ids_are_uuid_strings() {
        let id = AgentId::generate();
        assert_eq!(roundtrip(&id), serde_json::json!(id.0.to_string()));
        assert_eq!(id.0.get_version_num(), 7);
    }

    #[test]
    fn error_codes_match_the_backend_architecture() {
        let names: Vec<String> = ErrorCode::ALL
            .iter()
            .map(|c| {
                serde_json::to_value(c)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            names,
            [
                "not_member",
                "agent_not_shared",
                "requests_not_accepted",
                "not_found",
                "peer_offline",
                "approval_expired",
                "denied",
                "rate_limited",
                "harness_unavailable",
                "harness_unsupported",
                "conflict",
                "invalid_params",
                "internal",
            ]
        );
        assert_eq!(ErrorCode::InvalidParams.exit_code(), 2);
        assert_eq!(ErrorCode::AgentNotShared.exit_code(), 3);
        assert_eq!(ErrorCode::NotFound.exit_code(), 4);
        assert_eq!(ErrorCode::PeerOffline.exit_code(), 5);
    }

    #[test]
    fn unknown_codes_from_a_newer_mac_read_as_internal() {
        let error: ApiError =
            serde_json::from_str(r#"{"code":"added_later","message":"m"}"#).unwrap();
        assert_eq!(error.code, ErrorCode::Internal);
    }

    #[test]
    fn omitted_params_read_as_an_empty_object() {
        let request: RpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"agents_list"}"#).unwrap();
        let _: crate::agent::AgentsList = request.parse_params().unwrap();
        let request = RpcRequest::new(
            RpcId::Number(8),
            &crate::agent::AgentStart {
                agent_id: AgentId::generate(),
            },
        )
        .unwrap();
        assert_eq!(request.method, "agent_start");
        let _: crate::agent::AgentStart = request.parse_params().unwrap();
    }

    #[test]
    fn rpc_success_and_failure_are_distinguished() {
        let ok: RpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).unwrap();
        assert!(matches!(ok, RpcResponse::Success { .. }));

        let err = RpcResponse::Failure {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(RpcId::String("a".into())),
            error: ApiError::new(ErrorCode::NotFound, "no such agent").into(),
        };
        let json = roundtrip(&err);
        assert_eq!(json["error"]["code"], RPC_APPLICATION_ERROR);
        assert_eq!(json["error"]["data"]["code"], "not_found");
    }
}
