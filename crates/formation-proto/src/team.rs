//! Owner: B (collaboration core).
//!
//! Team members, invitations, and identity (FR-008, D-046). The creator's Mac holds
//! the authoritative, versioned roster (tech spec, "Team and roster").

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::common::{InvitationId, MemberId, Timestamp, commands};

/// The team, chosen by the creator's Mac when the team is created. Invitation links
/// carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamId(pub Uuid);

impl TeamId {
    /// A new time-ordered ID (UUIDv7, as the data model requires).
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

impl fmt::Display for TeamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TeamId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MemberRole {
    /// Created the team; manages the roster.
    Creator,
    Member,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Member {
    pub id: MemberId,
    /// Unique per team.
    pub github_login: String,
    pub display_name: String,
    /// Must match Tailscale WhoIs on every peer request, with `tailscale_node`.
    pub tailscale_login: String,
    /// The node's Tailscale `StableID` (see [`WhoIs::node`]).
    pub tailscale_node: String,
    pub role: MemberRole,
    pub joined_at: Timestamp,
    pub online: bool,
    pub last_seen_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Invitation {
    pub id: InvitationId,
    /// The short code, such as `7F3K-Q9PX`. Works once, until `expires_at`.
    pub code: String,
    pub created_by: MemberId,
    /// 24 hours after the invitation is made.
    pub expires_at: Timestamp,
    pub used_by: Option<MemberId>,
    pub used_at: Option<Timestamp>,
    /// What the invitee pastes: `formation://join/<code>?host=<host>&port=<port>&team=<team id>`,
    /// carrying the creator's tailnet address (tech spec, "Team and roster").
    #[serde(default)]
    pub link: String,
}

/// The single team this Mac belongs to (one team in the first version).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Team {
    pub me: MemberId,
    pub members: Vec<Member>,
    /// The open invitation, shown to the creator.
    pub invitation: Option<Invitation>,
}

/// Tailscale WhoIs on a peer connection. A tagged node has no user, so it has no
/// `WhoIs` and is never a member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WhoIs {
    /// `UserProfile.LoginName`.
    pub login: String,
    /// `Node.StableID`, which survives a rename, unlike the node's name.
    pub node: String,
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Creates the team with me as creator. My GitHub login comes from the `gh` session
/// in the MVP (backend architecture, "Identity and team").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamCreate {
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamInvite {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamJoin {
    /// What the invitee pasted: the invitation link ([`Invitation::link`]). A bare
    /// code has no address, so it is refused.
    pub code: String,
    pub display_name: Option<String>,
}

/// Creator only. Ends the member's access at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamRemove {
    pub member_id: MemberId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamLeave {}

commands! {
    TeamCreate => "team_create" -> Team;
    TeamInvite => "team_invite" -> Invitation;
    TeamJoin => "team_join" -> Team;
    TeamRemove => "team_remove" -> Team;
    TeamLeave => "team_leave" -> ();
}

/// `team.update`: a member joined, left, was removed, or the invitation changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamUpdateEvent {
    pub team: Team,
}

#[cfg(test)]
pub(crate) mod samples {
    use super::*;
    use crate::testutil::t0;

    pub fn member(login: &str, role: MemberRole) -> Member {
        Member {
            id: MemberId::generate(),
            github_login: login.into(),
            display_name: login.into(),
            tailscale_login: format!("{login}@github"),
            tailscale_node: format!("{login}-mbp"),
            role,
            joined_at: t0(),
            online: true,
            last_seen_at: Some(t0()),
        }
    }

    pub fn team() -> Team {
        let me = member("dohyeon", MemberRole::Creator);
        let mate = member("baekdusan", MemberRole::Member);
        Team {
            me: me.id,
            invitation: Some(Invitation {
                id: InvitationId::generate(),
                code: "7F3K-Q9PX".into(),
                created_by: me.id,
                expires_at: t0(),
                used_by: None,
                used_at: None,
                link: format!(
                    "formation://join/7F3K-Q9PX?host=dohyeon-mbp.tail0000.ts.net&port=7413&team={}",
                    TeamId::generate()
                ),
            }),
            members: vec![me, mate],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::team;
    use super::*;
    use crate::testutil::roundtrip;

    #[test]
    fn team_round_trips() {
        let team = team();
        let json = roundtrip(&TeamUpdateEvent { team: team.clone() });
        assert_eq!(json["team"]["members"][0]["role"], "creator");
        assert!(
            json["team"]["invitation"]["link"]
                .as_str()
                .is_some_and(|link| link.starts_with("formation://join/7F3K-Q9PX?"))
        );
        roundtrip(&TeamJoin {
            code: "formation://join/7F3K-Q9PX?host=100.64.0.1&port=7413&team=x".into(),
            display_name: None,
        });
        roundtrip(&TeamRemove {
            member_id: team.members[1].id,
        });
        roundtrip(&WhoIs {
            login: "baekdusan@github".into(),
            node: "nV3Ak1gn8j11CNTRL".into(),
        });
        let id = TeamId::generate();
        assert_eq!(roundtrip(&id), serde_json::json!(id.to_string()));
        assert_eq!(id.to_string().parse::<TeamId>().unwrap(), id);
    }

    #[test]
    fn an_invitation_without_a_link_still_reads() {
        // Additive field: JSON written before `link` existed still decodes.
        let mut json = serde_json::to_value(team().invitation.unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("link");
        let invitation: Invitation = serde_json::from_value(json).unwrap();
        assert_eq!(invitation.link, "");
    }
}
