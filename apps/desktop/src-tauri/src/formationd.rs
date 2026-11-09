//! The app's one connection to formationd's local socket (tech spec, "UI ↔
//! formationd"): newline-delimited JSON-RPC 2.0. Every window shares it. A call
//! is forwarded as a request and resolved by its reply; every `event`
//! notification goes to all windows. When the socket closes, pending calls fail
//! with `disconnected` and the connection retries with backoff, so the UI takes a
//! new snapshot when it comes back.
//!
//! This module knows nothing about Tauri; `lib.rs` connects a [`Sink`] that
//! emits to the windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::oneshot;

/// How long a call waits for its reply before failing with `timeout`.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_MIN: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// The socket to use: `FORMATION_SOCKET` if set; the fake's socket when
/// `FORMATION_FAKE` includes `daemon`; otherwise formationd's default path
/// (backend architecture, "Configuration, logs, and operations").
pub fn socket_path(socket: Option<&str>, fake: Option<&str>, home: &Path) -> PathBuf {
    if let Some(path) = socket.filter(|s| !s.is_empty()) {
        return PathBuf::from(path);
    }
    let support = home.join("Library/Application Support/Formation");
    if fake.is_some_and(|v| v.split(',').any(|part| part.trim() == "daemon")) {
        support.join("fake-formationd.sock")
    } else {
        support.join("formationd.sock")
    }
}

/// Whether the app can reach formationd, and where it looks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub connected: bool,
    pub socket: String,
}

/// Why a call failed. `rpc` carries formationd's JSON-RPC error; for
/// Formation errors `data` is the `ApiError` (`{ code, message }`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallError {
    pub code: &'static str,
    pub message: String,
    pub rpc_code: Option<i64>,
    pub data: Option<Box<Value>>,
}

impl CallError {
    fn disconnected() -> Self {
        Self::local("disconnected", "formationd is not reachable")
    }

    fn local(code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.to_owned(),
            rpc_code: None,
            data: None,
        }
    }
}

/// Where the connection delivers what formationd sends.
pub trait Sink: Send + Sync + 'static {
    /// An `event` notification's params (an `EventEnvelope`).
    fn event(&self, envelope: Value);
    fn status(&self, status: Status);
}

type Reply = Result<Value, CallError>;

struct Inner {
    path: PathBuf,
    writer: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next_id: AtomicU64,
    connected: AtomicBool,
}

/// A handle to the shared connection; cheap to clone.
#[derive(Clone)]
pub struct Connection {
    inner: Arc<Inner>,
}

impl Connection {
    /// Starts connecting in the background on the current tokio runtime.
    pub fn start(path: PathBuf, sink: impl Sink) -> Self {
        let conn = Connection {
            inner: Arc::new(Inner {
                path,
                writer: tokio::sync::Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                connected: AtomicBool::new(false),
            }),
        };
        let inner = Arc::clone(&conn.inner);
        tokio::spawn(run(inner, Arc::new(sink)));
        conn
    }

    pub fn status(&self) -> Status {
        Status {
            connected: self.inner.connected.load(Ordering::SeqCst),
            socket: self.inner.path.display().to_string(),
        }
    }

    /// Sends one JSON-RPC request and waits for its reply.
    pub async fn call(&self, method: &str, params: Value) -> Reply {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.inner.pending).insert(id, tx);
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let sent = {
            let mut writer = self.inner.writer.lock().await;
            match writer.as_mut() {
                Some(w) => w.write_all(format!("{line}\n").as_bytes()).await.is_ok(),
                None => false,
            }
        };
        if !sent {
            lock(&self.inner.pending).remove(&id);
            return Err(CallError::disconnected());
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(CallError::disconnected()),
            Err(_) => {
                lock(&self.inner.pending).remove(&id);
                Err(CallError::local(
                    "timeout",
                    "formationd did not answer in time",
                ))
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Connects, reads until the socket closes, and retries, forever.
async fn run(inner: Arc<Inner>, sink: Arc<dyn Sink>) {
    let mut retry = RETRY_MIN;
    loop {
        if let Ok(stream) = UnixStream::connect(&inner.path).await {
            retry = RETRY_MIN;
            let (read, write) = stream.into_split();
            *inner.writer.lock().await = Some(write);
            inner.connected.store(true, Ordering::SeqCst);
            sink.status(status(&inner, true));

            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                dispatch(&inner, sink.as_ref(), &line);
            }

            *inner.writer.lock().await = None;
            inner.connected.store(false, Ordering::SeqCst);
            for (_, tx) in lock(&inner.pending).drain() {
                let _ = tx.send(Err(CallError::disconnected()));
            }
            sink.status(status(&inner, false));
        }
        tokio::time::sleep(retry).await;
        retry = (retry * 2).min(RETRY_MAX);
    }
}

fn status(inner: &Inner, connected: bool) -> Status {
    Status {
        connected,
        socket: inner.path.display().to_string(),
    }
}

/// Routes one line from formationd: a reply resolves its call, an `event`
/// notification goes to the sink, anything else is ignored.
fn dispatch(inner: &Inner, sink: &dyn Sink, line: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let Some(tx) = lock(&inner.pending).remove(&id) else {
            return;
        };
        let reply = match msg.get("error").filter(|e| !e.is_null()) {
            Some(err) => Err(CallError {
                code: "rpc",
                message: err["message"].as_str().unwrap_or("error").to_owned(),
                rpc_code: err["code"].as_i64(),
                data: err
                    .get("data")
                    .filter(|d| !d.is_null())
                    .cloned()
                    .map(Box::new),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(reply);
    } else if msg["method"] == formation_proto_event_method() {
        sink.event(msg.get("params").cloned().unwrap_or(Value::Null));
    }
}

/// `formation_proto::common::EVENT_NOTIFICATION`, kept as a literal so the app
/// does not link the contract crate at run time.
const fn formation_proto_event_method() -> &'static str {
    "event"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_override_wins_then_fake_then_default() {
        let home = Path::new("/Users/me");
        assert_eq!(
            socket_path(Some("/tmp/x.sock"), Some("daemon"), home),
            PathBuf::from("/tmp/x.sock")
        );
        assert_eq!(
            socket_path(None, Some("claude,daemon"), home),
            PathBuf::from("/Users/me/Library/Application Support/Formation/fake-formationd.sock")
        );
        assert_eq!(
            socket_path(Some(""), None, home),
            PathBuf::from("/Users/me/Library/Application Support/Formation/formationd.sock")
        );
    }
}
