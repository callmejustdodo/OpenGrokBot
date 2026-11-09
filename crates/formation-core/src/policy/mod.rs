//! Policy: membership and sharing checks, always-allowed reads, rules, approvals
//! and expiry, chain limits (backend architecture, "Where each rule is enforced").
//!
//! - [`reads`] decides which tool calls are always-allowed reads (D-028).
//! - [`approvals`] raises approvals, takes the owner's allow once or deny, and
//!   expires them after 24 hours (FR-015, US-011).

pub mod approvals;
pub mod reads;
