//! Owner: B. One ordered queue per agent for owner messages, teammates' work
//! requests, and schedule runs. A's executor is its single consumer per agent
//! (TASK-023, TASK-024).

use async_trait::async_trait;

use formation_proto::common::{AgentId, ApiError, MessageId};
use formation_proto::request::{QueueItem, QueueSource};

use super::Result;

/// What the executor reports about an item it took.
#[derive(Debug, Clone, PartialEq)]
pub enum RunProgress {
    Started,
    Completed { result: Option<String> },
    Failed { error: ApiError },
    Interrupted,
}

#[async_trait]
pub trait RequestQueue: Send + Sync {
    /// Appends a message; publishes `agent.queue`.
    async fn enqueue(
        &self,
        agent_id: AgentId,
        source: QueueSource,
        text: String,
    ) -> Result<QueueItem>;

    async fn items(&self, agent_id: AgentId) -> Result<Vec<QueueItem>>;

    /// Edits a queued message and publishes `agent.queue`.
    async fn edit(&self, agent_id: AgentId, id: MessageId, text: String) -> Result<QueueItem>;

    /// Removes a message that has not started.
    async fn withdraw(&self, agent_id: AgentId, id: MessageId) -> Result<()>;

    /// Removes and returns one message so it can steer the running turn. The caller
    /// restores it if `Harness::steer` fails.
    async fn take(&self, agent_id: AgentId, id: MessageId) -> Result<QueueItem>;

    /// Restores an item returned by `take` with the same `MessageId`, at its
    /// original position ordered by `queued_at`, and publishes `agent.queue`.
    async fn restore(&self, agent_id: AgentId, item: QueueItem) -> Result<()>;

    /// Waits for the agent's next message. Single consumer per agent.
    async fn next(&self, agent_id: AgentId) -> Result<QueueItem>;

    /// Updates the request behind the item (if any) and tells the requester.
    async fn report(&self, id: MessageId, progress: RunProgress) -> Result<()>;
}
