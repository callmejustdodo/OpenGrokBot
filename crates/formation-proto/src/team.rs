//! Owner: B (collaboration core).
//!
//! Team members, invitations, and identity (FR-008, D-046).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::{InvitationId, MemberId, Timestamp, commands};

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
    /// Must match Tailscale WhoIs on every peer request.
    pub tailscale_login: String,
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
    /// What the invitee pastes. Carries the creator's tailnet address; expires.
    pub code: String,
    pub created_by: MemberId,
    pub expires_at: Timestamp,
    pub used_by: Option<MemberId>,
    pub used_at: Option<Timestamp>,
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

/// Tailscale WhoIs on a peer connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WhoIs {
    pub login: String,
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
                code: "fmn1-abcd".into(),
                created_by: me.id,
                expires_at: t0(),
                used_by: None,
                used_at: None,
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
        roundtrip(&TeamJoin {
            code: "fmn1-abcd".into(),
            display_name: None,
        });
        roundtrip(&TeamRemove {
            member_id: team.members[1].id,
        });
        roundtrip(&WhoIs {
            login: "baekdusan@github".into(),
            node: "baekdusan-mbp".into(),
        });
    }
}
