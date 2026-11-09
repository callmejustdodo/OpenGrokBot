//! Team creation and joining (TASK-017; FR-008, D-046). The model is in the tech
//! spec, "Team and roster", and identity in the backend architecture, "Identity and
//! team".
//!
//! - The creator's Mac holds the authoritative roster with a version
//!   ([`roster`]). Members' Macs keep a copy: the peer server applies each
//!   `PeerEvent::Roster` push, and `GET /v1/team` on reconnect, through
//!   [`TeamService::apply_roster`]; removals apply first.
//! - Invitations are single-use codes such as `7F3K-Q9PX` ([`code`]) that expire
//!   after 24 hours, shared as a link carrying the creator's tailnet address, port,
//!   and team ([`link`]).
//! - `POST /v1/team/join` ([`TeamService::handle_join`]) is the only peer call that
//!   skips the member check. Tailscale WhoIs is still required, and the new
//!   member's Tailscale login and node come from it, never from the body. Bad codes
//!   are rate-limited ([`limit`]). The GitHub login is what the joiner's Mac read
//!   from `gh api user` (MVP).
//! - Every other peer call maps WhoIs to a member by recorded login and node
//!   ([`identity::authenticate`]); tagged and unknown nodes are refused.
//! - State lives behind the `TeamRepo` part of the `CollabStore` port; the SQL for
//!   B's SQLite store is [`MIGRATIONS`].

pub mod code;
mod commands;
pub mod identity;
mod join;
pub mod limit;
pub mod link;
pub mod roster;
mod service;

#[cfg(test)]
mod tests;

pub use identity::authenticate;
pub use limit::JoinLimit;
pub use link::InvitationLink;
pub use roster::RosterChange;
pub use service::{
    Clock, INVITATION_TTL, LocalIdentity, TailnetSelf, TeamConfig, TeamService, system_clock,
};

use crate::ports::Migration;

/// B's team schema, for A's migration runner (TASK-011). Versions are in
/// [`crate::ports::MIGRATIONS_B`].
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 2000,
    name: "team",
    sql: include_str!("migrations/2000_team.sql"),
}];
