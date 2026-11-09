//! The roster and how it spreads (tech spec, "Team and roster").
//!
//! The creator's Mac holds the authoritative roster with a version that starts at 1
//! and goes up by one on every change (join, removal). Each change is published as
//! a [`RosterChange`]: the peer server closes the connections of removed members
//! and pushes `PeerEvent::Roster` to the others. Members' Macs apply a roster from
//! that push, and from `GET /v1/team` when they reconnect, only when its version is
//! newer than the one they hold, and apply removals before anything else.

use std::collections::HashSet;

use formation_proto::common::{ApiError, ErrorCode, MemberId};
use formation_proto::peer::{PeerEvent, RosterResponse};
use formation_proto::team::{Member, MemberRole};

use crate::ports::Result;

/// One roster change, on the Mac where it was made or applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterChange {
    pub version: u64,
    /// The roster after the change. Empty on a Mac that was itself removed.
    pub members: Vec<Member>,
    /// Members whose access ended with this change. The peer server closes their
    /// open connections.
    pub removed: Vec<MemberId>,
}

impl RosterChange {
    /// The push for members' Macs on `WS /v1/events`.
    pub fn to_peer_event(&self) -> PeerEvent {
        PeerEvent::Roster {
            members: self.members.clone(),
            version: self.version,
        }
    }

    pub fn to_response(&self) -> RosterResponse {
        RosterResponse {
            members: self.members.clone(),
            version: self.version,
        }
    }
}

/// A roster received from another Mac is consistent: unique member IDs, GitHub
/// logins (ignoring case), and Tailscale logins, and exactly one creator. Returns
/// the creator.
pub(crate) fn check(members: &[Member]) -> Result<&Member> {
    let invalid = |message: &str| ApiError::new(ErrorCode::InvalidParams, message);
    let mut ids = HashSet::new();
    let mut github = HashSet::new();
    let mut tailscale = HashSet::new();
    for member in members {
        if !ids.insert(member.id)
            || !github.insert(member.github_login.to_ascii_lowercase())
            || !tailscale.insert(member.tailscale_login.as_str())
        {
            return Err(invalid("the roster lists a member twice"));
        }
    }
    let mut creators = members.iter().filter(|m| m.role == MemberRole::Creator);
    match (creators.next(), creators.next()) {
        (Some(creator), None) => Ok(creator),
        _ => Err(invalid("the roster must have exactly one creator")),
    }
}
