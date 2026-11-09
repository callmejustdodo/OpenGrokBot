//! Ports: the traits a module depends on instead of another module's internals
//! (implementation plan, "How the parts join"). formationd wires the real adapters;
//! tests and early work use the fakes.
//!
//! | Trait | Owner | Real adapter | Fake |
//! | --- | --- | --- | --- |
//! | [`Harness`] | A | `harness::claude` | `FakeHarness` (TASK-003) |
//! | [`Store`] | A | `store` (SQLite) | in-memory store (TASK-003) |
//! | [`Notifier`] | A | `notify` | — |
//! | [`EventSink`] | A | `service` event bus | — |
//! | [`CommandHandler`] | A defines; each role implements one | `service` routes to them | each role's fake |
//! | [`CollabStore`] | B | B's SQLite repositories | in-memory store (TASK-042) |
//! | [`Policy`] | B | `policy` | `FakePolicy` (TASK-042) |
//! | [`PermissionGate`] | B | `permission` | `FakePermissionGate` (TASK-042) |
//! | [`Peers`] | B | `peer` | `FakePeers` (TASK-042) |
//! | [`RequestQueue`] | B | `requests` | `FakeRequestQueue` (TASK-042) |
//!
//! Every port returns [`formation_proto::common::ApiError`] so errors reach clients
//! with their code unchanged.

mod commands;
mod events;
mod harness;
mod notifier;
mod peers;
mod permission;
mod policy;
mod requests;
mod store;

pub use commands::{CommandCall, CommandHandler};
pub use events::EventSink;
pub use harness::{ForkResult, ForkRun, ForkTools, Harness, RunInfo, TurnResult};
pub use notifier::Notifier;
pub use peers::Peers;
pub use permission::{PermissionGate, PermissionRequest};
pub use policy::{Policy, ToolContext};
pub use requests::{RequestQueue, RunProgress};
pub use store::{
    ActivityRepo, AgentRepo, ApprovalRepo, AttentionRepo, CollabStore, MIGRATIONS_A, MIGRATIONS_B,
    Migration, QuestionRepo, RequestRepo, RuleRepo, ScheduleRepo, SessionRepo, SettingsRepo,
    ShareRepo, Store, TeamRepo,
};

pub type Result<T> = std::result::Result<T, formation_proto::common::ApiError>;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Fakes are swapped in by configuration, so every port must work as a trait object.
    #[test]
    fn ports_are_dyn_compatible() {
        let _: Option<Arc<dyn Harness>> = None;
        let _: Option<Arc<dyn Store>> = None;
        let _: Option<Arc<dyn Notifier>> = None;
        let _: Option<Arc<dyn EventSink>> = None;
        let _: Option<Arc<dyn CommandHandler>> = None;
        let _: Option<Arc<dyn CollabStore>> = None;
        let _: Option<Arc<dyn Policy>> = None;
        let _: Option<Arc<dyn PermissionGate>> = None;
        let _: Option<Arc<dyn Peers>> = None;
        let _: Option<Arc<dyn RequestQueue>> = None;
    }
}
