//! Local JSON-RPC service, snapshot assembly, and ordered event fan-out.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use formation_proto::common::*;
use formation_proto::events::{Event, EventEnvelope};
use formation_proto::policy::Caller;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::ports::{CommandCall, CommandHandler, EventSink};

mod snapshot;
mod socket;
pub use snapshot::SnapshotBuilder;
pub use socket::{SocketServer, connection, default_socket_path};

/// Serializes numbering and publication so concurrent publishers cannot reorder events.
pub struct EventBus {
    seq: Mutex<Seq>,
    sender: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            seq: Mutex::new(Seq(0)),
            sender,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.sender.subscribe()
    }

    pub fn last_seq(&self) -> Seq {
        *self.seq.lock().unwrap()
    }
}

impl EventSink for EventBus {
    fn publish(&self, event: Event) -> Seq {
        let mut seq = self.seq.lock().unwrap();
        *seq = seq.next();
        let _ = self.sender.send(EventEnvelope {
            seq: *seq,
            at: std::time::SystemTime::now().into(),
            event,
        });
        *seq
    }
}

pub struct Router {
    handlers: HashMap<&'static str, Arc<dyn CommandHandler>>,
}

impl Router {
    /// `snapshot` is reserved for the service, including when no handlers are registered.
    pub fn new(handlers: Vec<Arc<dyn CommandHandler>>) -> Result<Self, String> {
        let mut routes = HashMap::new();
        for handler in handlers {
            for method in handler.methods() {
                if method == "snapshot" || routes.insert(method, handler.clone()).is_some() {
                    return Err(format!("duplicate method: {method}"));
                }
            }
        }
        Ok(Self { handlers: routes })
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let handler = self
            .handlers
            .get(method)
            .ok_or_else(|| rpc_error(RPC_METHOD_NOT_FOUND, "Method not found"))?;
        handler
            .handle(CommandCall {
                caller: Caller::Owner,
                method: method.into(),
                params,
            })
            .await
            .map_err(RpcError::from)
    }
}

fn rpc_error(code: i32, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

fn decode_request(line: &[u8]) -> Result<RpcRequest, (Option<RpcId>, RpcError)> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|_| (None, rpc_error(RPC_PARSE_ERROR, "Parse error")))?;
    let id = value
        .get("id")
        .and_then(|id| serde_json::from_value(id.clone()).ok());
    let request: RpcRequest = serde_json::from_value(value).map_err(|_| {
        (
            id.clone(),
            rpc_error(RPC_INVALID_REQUEST, "Invalid request"),
        )
    })?;
    if request.jsonrpc != JSONRPC_VERSION
        || !(request.params.is_null() || request.params.is_object() || request.params.is_array())
    {
        return Err((id, rpc_error(RPC_INVALID_REQUEST, "Invalid request")));
    }
    Ok(request)
}

#[cfg(test)]
mod tests;
