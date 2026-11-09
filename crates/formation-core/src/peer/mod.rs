//! Peer protocol between formationd instances over the tailnet.
//!
//! This is the TASK-007 spike from `documentations/implementation_plan.md`:
//! two Macs reach each other on port 7413 and each request is tied to the
//! caller's Tailscale user and node (D-045, D-046). The `peer_spike` example
//! runs it. TASK-018 builds the real HTTP and WebSocket server; the WhoIs
//! client and the address rules here carry over.
//!
//! - [`tailscale`] finds this Mac's Tailscale (LocalAPI, else CLI) and asks it
//!   who owns a TCP peer address.
//! - [`identity`] is the typed WhoIs answer and the fail-closed check.
//! - [`server`] binds the Tailscale address and answers `GET /spike/whoami`
//!   with the caller's identity, or `403`. It serves none of the contract's
//!   `/v1` endpoints; any other path gets `404`.
//! - [`github`] reads the GitHub login from an existing `gh` session.
//!
//! What the spike established (Tailscale 1.104.2, Mac App Store build). A Node
//! prototype ran first on one Mac and then between two Macs; this Rust code has
//! run on one Mac, with the Mac calling its own Tailscale address, under a
//! launchd-like environment. The two-Mac run of this code is still to do.
//!
//! - WhoIs is reachable from Rust without Tailscale's Rust bindings (which lack
//!   it, tailscale-rs #478). The LocalAPI answers in about a millisecond; the
//!   CLI takes 25 to 75 ms because it starts a process. Both return the same
//!   `apitype.WhoIsResponse`, and on this Mac they agreed.
//! - The App Store build has no Unix socket: its LocalAPI is a loopback TCP
//!   port whose token is in a file name (see [`tailscale`]), and both change
//!   when the extension restarts.
//! - Under launchd the `PATH` is minimal, and unless `TAILSCALE_BE_CLI=1` is
//!   set the GUI app's binary prints a GUI error instead of running the
//!   command, yet exits 0. Every program is run by absolute path and judged by
//!   its output.
//! - The login is the same on every node of one Tailscale account, so it names
//!   a person, not a Mac. The node's `StableID` survives renames; its name does
//!   not. A tagged node has no user at all and is refused.
//! - Binding the Tailscale IPv4 address alone makes loopback and LAN
//!   addresses refuse the connection outright. Identity comes from the TCP
//!   peer address; headers such as `X-Forwarded-For` are ignored.
//! - WhoIs identifies a machine, not a process: another process or OS user on
//!   the same Mac that connects to its Tailscale address is reported as that
//!   Mac's owner. The server marks such calls `self_connection`; what the real
//!   server allows from itself is for TASK-018.
//!
//! Left open: whether the LocalAPI survives quitting the Tailscale GUI; lsof
//! discovery inside a real LaunchAgent; the other members' Tailscale builds
//! (the standalone build's token file is readable only by admins); the
//! tailnet's IPv6 address, which the spike does not bind.
//!
//! TASK-018 adds the protocol itself:
//! - [`api`] is the owner's side: the HTTP and WebSocket server on the Tailscale
//!   address. Every call is tied to a member through WhoIs and passes policy
//!   before it reaches the `team`, `ask`, or `requests` handler behind it.
//! - [`client`] is the asker's side: [`client::PeerClient`] implements the
//!   [`Peers`](crate::ports::Peers) port over HTTP and keeps one `WS /v1/events`
//!   connection per teammate.
//! - [`outbox`] keeps questions and work requests for a teammate whose Mac is
//!   offline and sends them in order when it is back (FR-016).
//! - [`presence`] turns connections opening and closing into presence events.
//!
//! #179 adds the real adapters formationd wires in: [`tailnet`] identifies
//! callers through Tailscale WhoIs and finds each teammate's peer server from
//! their node in Tailscale's peer list.

pub mod api;
pub mod client;
pub mod github;
pub(crate) mod http;
pub mod identity;
pub mod outbox;
pub mod presence;
pub(crate) mod process;
pub mod server;
pub mod tailnet;
pub mod tailscale;

pub use identity::{
    DenyReason, PeerIdentity, Resolved, TailnetNode, TailnetUser, WhoIsError, WhoIsResolver,
    WhoIsSource,
};
pub use server::{is_tailscale_ip, is_tailscale_ipv4};
pub use tailnet::{DirectoryConfig, NodeAddressSource, TailnetDirectory, TailscaleCallers};
pub use tailscale::{NodeAddresses, SelfNode, Tailscale, Via};

#[cfg(test)]
mod loopback_test;
