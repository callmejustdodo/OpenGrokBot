//! The activity history (FR-020): `history_list` and `history_delete` over the
//! store's `ActivityRepo`, and a helper for recording this Mac's own activity.

use std::sync::Arc;

use async_trait::async_trait;
use formation_proto::{
    agent::{Activity, ActivityKind, AttentionTarget, HistoryDelete, HistoryList},
    common::{ActivityId, AgentId, ApiError, ErrorCode, Timestamp},
    policy::Caller,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ports::{ActivityRepo, CommandCall, CommandHandler, Result, Store};

/// Records one activity whose actor is this Mac's owner.
pub fn record<R: ActivityRepo + ?Sized>(
    repo: &R,
    kind: ActivityKind,
    agent_id: Option<AgentId>,
    target: Option<AttentionTarget>,
    at: Timestamp,
    summary: String,
) -> Result<()> {
    repo.record_activity(&Activity {
        id: ActivityId::generate(),
        kind,
        actor: None,
        agent_id,
        target,
        at,
        summary,
    })
}

pub struct History {
    store: Arc<dyn Store>,
}

impl History {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl CommandHandler for History {
    fn methods(&self) -> Vec<&'static str> {
        vec!["history_list", "history_delete"]
    }

    async fn handle(&self, call: CommandCall) -> Result<Value> {
        if call.caller != Caller::Owner {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "history requires the owner",
            ));
        }
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || match call.method.as_str() {
            "history_list" => {
                let args: HistoryList = decode(call.params)?;
                let items =
                    store.activity(args.agent_id, args.member_id, args.before, args.limit)?;
                serde_json::to_value(items)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
            }
            "history_delete" => {
                let args: HistoryDelete = decode(call.params)?;
                store.delete_activity(&args.ids)?;
                Ok(Value::Null)
            }
            _ => Err(ApiError::new(
                ErrorCode::InvalidParams,
                "unknown history command",
            )),
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| ApiError::new(ErrorCode::InvalidParams, e.to_string()))
}
