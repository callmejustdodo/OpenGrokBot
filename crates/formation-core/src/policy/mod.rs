//! Policy: membership and sharing checks, visibility, always-allowed reads, rules,
//! approvals and expiry, chain limits (backend architecture, "Where each rule is
//! enforced").
//!
//! - [`reads`] decides which tool calls are always-allowed reads (D-028).
//! - [`approvals`] raises approvals, takes the owner's allow once or deny, and
//!   expires them after 24 hours (FR-015, US-011).
//! - [`sharing`] holds per-agent share settings and what a teammate may see.
//! - [`team_policy`] is the real [`Policy`](crate::ports::Policy) built from them.

pub mod approvals;
pub mod reads;
pub mod sharing;
pub mod team_policy;

pub use team_policy::TeamPolicy;
