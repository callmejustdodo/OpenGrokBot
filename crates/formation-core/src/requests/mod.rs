//! Work requests (FR-014, FR-017, FR-019; TASK-023) and the per-agent queue that
//! A's executor consumes through the [`RequestQueue`](crate::ports::RequestQueue)
//! port.
//!
//! - [`Requests`], on the owner's Mac: one ordered queue per agent for owner
//!   messages, schedule runs, and teammates' requests; creation with idempotency;
//!   the "accepts requests" check; cancel before start; restart recovery.
//! - [`endpoints`]: the handlers for `POST /v1/requests`, `GET /v1/requests/{id}`,
//!   and `POST /v1/requests/{id}/cancel` that the peer server calls.
//! - [`RequestClient`], on the requester's Mac: the `request_create`,
//!   `request_get`, and `request_cancel` commands, sent through the
//!   [`Peers`](crate::ports::Peers) port, with the chain taken from `from_run`.
//! - [`status`]: the status machine both sides follow.
//!
//! The requests module only queues and tracks. The executor runs each request in a
//! fork of the agent's session whose tools go through the permission gate (#14,
//! `ForkTools::Gated`), and reports back with [`RequestQueue::report`](crate::ports::RequestQueue::report).

mod client;
pub mod endpoints;
mod queue;
pub mod status;

pub use client::RequestClient;
pub use queue::Requests;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;
