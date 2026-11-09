//! Joining: the invitee's `team_join` command and the creator's
//! `POST /v1/team/join` handler.

use formation_proto::common::{ApiError, ErrorCode, MemberId};
use formation_proto::peer::{PeerJoin, PeerJoinAccepted};
use formation_proto::team::{Invitation, Member, MemberRole, Team, TeamJoin, WhoIs};

use super::code;
use super::identity;
use super::link::InvitationLink;
use super::roster::{self, RosterChange};
use super::service::{TeamService, conflict};
use crate::ports::{Result, TeamRecord, TeamRepo};

impl<S: TeamRepo + Send + Sync + ?Sized> TeamService<S> {
    /// `POST /v1/team/join` on the creator's Mac: the one peer call that skips the
    /// member check. `caller` is the connection's Tailscale WhoIs (`None` for a
    /// tagged or unknown node); the new member's Tailscale login and node come from
    /// it, never from the body.
    ///
    /// In order: `not_found` on a Mac that is not the creator's; `not_member` for a
    /// caller without a Tailscale user; `rate_limited` after too many bad codes;
    /// `invalid_params` for a malformed GitHub login; `conflict` when the caller's
    /// Tailscale or GitHub account is already a member; `not_found` for an unknown,
    /// used, or expired code, which counts as a bad attempt. A caller retrying its
    /// own successful join gets the same answer again.
    pub fn handle_join(&self, caller: Option<&WhoIs>, join: &PeerJoin) -> Result<PeerJoinAccepted> {
        let mut limiter = self.lock();
        if !self.is_creator()? {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                "this Mac does not take joins; they go to the team creator's Mac",
            ));
        }
        let record = self.record()?;
        let whois = identity::person(caller)?;
        let now = self.now();
        limiter.check(&whois.login, now)?;
        let github_login = identity::github_login(&join.github_login)?;
        let display_name = identity::display_name(&join.display_name, &github_login);
        let invitation = match code::normalize(&join.code) {
            Some(code) => self.store.invitation_by_code(&code)?,
            None => None,
        };

        if let Some(member) = self.store.member_by_tailscale(&whois.login)? {
            let retry = member.tailscale_node == whois.node
                && invitation
                    .as_ref()
                    .is_some_and(|i| i.used_by == Some(member.id));
            if retry {
                return self.accepted(&record, member.id);
            }
            return Err(conflict(
                "this Tailscale account is already a member of the team",
            ));
        }

        let invitation = match invitation {
            Some(i) if i.used_by.is_none() && i.expires_at > now => i,
            refused => {
                let message = match refused {
                    None => "no such invitation; check the link or ask for a new one",
                    Some(i) if i.used_by.is_some() => {
                        "this invitation was already used; ask for a new link"
                    }
                    Some(_) => "this invitation has expired; ask for a new link",
                };
                limiter.record_failure(&whois.login, now);
                return Err(ApiError::new(ErrorCode::NotFound, message));
            }
        };

        if self
            .store
            .members()?
            .iter()
            .any(|m| m.github_login.eq_ignore_ascii_case(&github_login))
        {
            return Err(conflict(
                "this GitHub account is already a member of the team",
            ));
        }
        let member = Member {
            id: MemberId::generate(),
            github_login,
            display_name,
            tailscale_login: whois.login.clone(),
            tailscale_node: whois.node.clone(),
            role: MemberRole::Member,
            joined_at: now,
            online: true,
            last_seen_at: Some(now),
        };
        self.store.put_member(&member)?;
        let mut record = record;
        if record.open_invitation.as_ref() == Some(&invitation.code) {
            record.open_invitation = None;
        }
        self.store.put_invitation(&Invitation {
            used_by: Some(member.id),
            used_at: Some(now),
            ..invitation
        })?;
        let record = self.commit_roster(record, vec![])?;
        self.changed(&record)?;
        self.accepted(&record, member.id)
    }

    fn accepted(&self, record: &TeamRecord, member_id: MemberId) -> Result<PeerJoinAccepted> {
        Ok(PeerJoinAccepted {
            member_id,
            members: self.store.members()?,
            team_id: Some(record.id),
            version: record.roster_version,
        })
    }

    /// `team_join` on the invitee's Mac: reads the pasted link, sends the code with
    /// this Mac's GitHub login to the creator's Mac, and stores the roster it gets
    /// back. `invalid_params` for a link that does not parse; `conflict` when this
    /// Mac is already in a team; the creator's errors (`not_found` for an expired
    /// link) and `peer_offline` come through unchanged.
    pub async fn join(&self, request: &TeamJoin) -> Result<Team> {
        if self.store.team()?.is_some() {
            return Err(conflict("this Mac is already in a team"));
        }
        let link = InvitationLink::parse(&request.code)?;
        let github_login = identity::github_login(&self.identity.github_login()?)?;
        let display_name =
            identity::display_name(request.display_name.as_deref().unwrap_or(""), &github_login);
        let join = PeerJoin {
            code: link.code.clone(),
            github_login,
            display_name,
        };
        let accepted = self.peers.join(&link.address(), join).await?;

        let bad_reply = |message: &str| {
            ApiError::new(
                ErrorCode::Internal,
                format!("the team creator's Mac sent a bad join reply: {message}"),
            )
        };
        if accepted.team_id.is_some_and(|id| id != link.team) {
            return Err(bad_reply("it is for another team"));
        }
        roster::check(&accepted.members).map_err(|error| bad_reply(&error.message))?;
        if !accepted.members.iter().any(|m| m.id == accepted.member_id) {
            return Err(bad_reply("the roster does not list the new member"));
        }

        let _guard = self.lock();
        if self.store.team()?.is_some() {
            return Err(conflict("this Mac is already in a team"));
        }
        for member in &accepted.members {
            self.store.put_member(member)?;
        }
        self.store.set_me(accepted.member_id)?;
        let record = TeamRecord {
            id: link.team,
            creator_host: link.host,
            creator_port: link.port,
            roster_version: accepted.version,
            open_invitation: None,
        };
        self.store.put_team(Some(&record))?;
        let team = self.changed(&record)?;
        // The joined roster is a roster change on this Mac like an applied one: it
        // is what tells `forward_roster`'s callback (the peer client) to open
        // links to the new teammates. Nothing else would, since the creator's
        // push travels over a link this Mac does not have yet. No receiver (no
        // peer server running) is fine.
        let _ = self.roster_tx.send(RosterChange {
            version: record.roster_version,
            members: self.store.members()?,
            removed: vec![],
        });
        Ok(team)
    }
}
