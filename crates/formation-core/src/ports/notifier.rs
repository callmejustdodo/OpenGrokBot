//! Owner: A. Needs-attention items (FR-007): the inbox and macOS notifications.

use async_trait::async_trait;

use formation_proto::agent::{Attention, AttentionKind, AttentionTarget};

use super::Result;

#[async_trait]
pub trait Notifier: Send + Sync {
    /// Raises an item, publishes `notify`, and asks the app to show a macOS
    /// notification unless muted. Opens the app hidden if it is not running.
    async fn raise(
        &self,
        kind: AttentionKind,
        target: AttentionTarget,
        subject: String,
    ) -> Result<Attention>;

    /// Resolves every open item for the target, for example when an approval is decided.
    async fn resolve(&self, target: AttentionTarget) -> Result<()>;

    /// Unresolved items.
    async fn inbox(&self) -> Result<Vec<Attention>>;
}
