//! Owner: B. Fakes of B's ports (TASK-042), so A can run the request executor and
//! C's fake service can simulate approvals without B's real modules.
//!
//! | Port | Fake | Default behavior |
//! | --- | --- | --- |
//! | [`CollabStore`](crate::ports::CollabStore) | [`MemoryCollabStore`] | Empty; keeps the repositories' documented rules (private sharing by default, insert-or-return on idempotency keys, removed members hidden) |
//! | [`Policy`](crate::ports::Policy) | [`FakePolicy`] | No members, nothing shared; reads allowed, every other tool call needs approval |
//! | [`PermissionGate`](crate::ports::PermissionGate) | [`FakePermissionGate`] | Permission tool prompts: reads allowed; every other call raises an approval and waits for [`FakePermissionGate::decide_approval`] or [`FakePermissionGate::expire`]. `PreToolUse` hook prompts: `defer` unless denied |
//! | [`Peers`](crate::ports::Peers) | [`FakePeers`] | Teammates you add, online, with their shared agents; questions are received, requests queued |
//! | [`RequestQueue`](crate::ports::RequestQueue) | [`FakeRequestQueue`] | Empty per-agent queues; one waiting consumer per agent |
//! | none yet (TASK-007) | [`StubWhoIs`], through [`WhoIsResolver`] | No addresses; unknown callers get `not_member` |
//!
//! Every fake is thread-safe. All but [`MemoryCollabStore`], which only keeps the
//! repositories' state, record each call for assertions (`calls()`) and can be
//! scripted to fail. None of them sleeps or reads the clock for timeouts: tests
//! trigger expiry and state changes themselves, so they stay deterministic.
//! [`samples`] builds the proto values the fakes take.

mod peers;
mod permission;
mod policy;
mod requests;
pub mod samples;
mod store;
mod whois;

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use formation_proto::common::{ApiError, ErrorCode, Timestamp};

pub use peers::{AskReply, FakePeers, OutboxItem, PeersCall};
pub use permission::{FakePermissionGate, GateAnswer};
pub use policy::{FakePolicy, PolicyCall, PolicyMethod};
pub use requests::{FakeRequestQueue, QueueCall, QueueMethod};
pub use store::MemoryCollabStore;
pub use whois::{StubIdentity, StubWhoIs, WhoIsResolver};

/// Tools the fakes treat as always-allowed reads (D-028). The real policy also
/// checks that the path is inside the agent folder or an assigned repository.
pub const READ_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS"];

/// How long an approval waits before it expires (FR-015). The fakes only stamp
/// `expires_at` with it; tests expire approvals explicitly.
pub const APPROVAL_TTL: Duration = Duration::from_secs(24 * 60 * 60);

fn is_read(tool_name: &str) -> bool {
    READ_TOOLS.contains(&tool_name)
}

/// Locks without propagating poison, so one panicking test thread does not fail
/// every later call on a shared fake.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now() -> Timestamp {
    Timestamp::from(SystemTime::now())
}

fn later(at: Timestamp, by: Duration) -> Timestamp {
    Timestamp::from(SystemTime::from(at) + by)
}

fn error(code: ErrorCode, message: impl Into<String>) -> ApiError {
    ApiError::new(code, message)
}

#[cfg(test)]
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}
