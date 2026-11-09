//! [`TeamService`]: the team on this Mac, behind the [`TeamRepo`] part of the
//! `CollabStore` port.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::broadcast;

use formation_proto::common::{ApiError, ErrorCode, InvitationId, MemberId, Timestamp};
use formation_proto::events::Event;
use formation_proto::peer::{PEER_PORT, RosterResponse};
use formation_proto::team::{Invitation, Member, MemberRole, Team, TeamId, TeamUpdateEvent, WhoIs};

use super::code;
use super::identity;
use super::limit::{JoinLimit, JoinLimiter};
use super::link::{self, InvitationLink};
use super::roster::{self, RosterChange};
use crate::ports::{EventSink, Peers, Result, TeamRecord, TeamRepo};

/// The current time; tests pass a clock they move by hand.
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| Timestamp::from(SystemTime::now()))
}

/// How long an invitation works.
pub const INVITATION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamConfig {
    /// The port this Mac's peer server listens on; invitation links carry it.
    pub peer_port: u16,
    pub invitation_ttl: Duration,
    pub join_limit: JoinLimit,
}

impl Default for TeamConfig {
    fn default() -> Self {
        Self {
            peer_port: PEER_PORT,
            invitation_ttl: INVITATION_TTL,
            join_limit: JoinLimit::default(),
        }
    }
}

/// This Mac on the tailnet, from Tailscale's own status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetSelf {
    /// This Mac's user login and node `StableID`, as teammates' WhoIs will see them.
    pub whois: WhoIs,
    /// The MagicDNS name or tailnet IP teammates reach this Mac at.
    pub host: String,
}

/// What this Mac knows about itself. formationd adapts TASK-007's Tailscale and `gh`
/// clients to it; tests use a stub. Calls may block (they run a CLI or ask the
/// LocalAPI); call them from blocking-safe code, like the repositories.
pub trait LocalIdentity: Send + Sync {
    fn tailnet_self(&self) -> Result<TailnetSelf>;
    /// The user's GitHub login: `gh api user` in the MVP.
    fn github_login(&self) -> Result<String>;
}

/// The team on this Mac: the creator's authoritative roster, or a member's copy.
///
/// Every change to the roster or the invitation is serialized by one lock, written
/// through the store, published as `team.update` on the event bus, and, for roster
/// changes, sent to [`TeamService::subscribe_roster`] for the peer server.
pub struct TeamService<S: TeamRepo + Send + Sync + ?Sized> {
    pub(super) store: Arc<S>,
    pub(super) identity: Arc<dyn LocalIdentity>,
    pub(super) peers: Arc<dyn Peers>,
    pub(super) events: Arc<dyn EventSink>,
    pub(super) clock: Clock,
    pub(super) config: TeamConfig,
    /// Held for every change; also holds the join rate limit.
    pub(super) state: Mutex<JoinLimiter>,
    pub(super) roster_tx: broadcast::Sender<RosterChange>,
}

impl<S: TeamRepo + Send + Sync + ?Sized> TeamService<S> {
    pub fn new(
        store: Arc<S>,
        identity: Arc<dyn LocalIdentity>,
        peers: Arc<dyn Peers>,
        events: Arc<dyn EventSink>,
        clock: Clock,
        config: TeamConfig,
    ) -> Self {
        Self {
            store,
            identity,
            peers,
            events,
            clock,
            config,
            state: Mutex::new(JoinLimiter::new(config.join_limit)),
            roster_tx: broadcast::channel(64).0,
        }
    }

    /// Roster changes made or applied on this Mac, for the peer server: on the
    /// creator's Mac it closes removed members' connections, then pushes
    /// [`RosterChange::to_peer_event`] to every member. On a member's Mac this
    /// includes the roster it received when it joined.
    pub fn subscribe_roster(&self) -> broadcast::Receiver<RosterChange> {
        self.roster_tx.subscribe()
    }

    /// The snapshot's `team`: `None` until this Mac creates or joins one.
    pub fn team(&self) -> Result<Option<Team>> {
        match self.store.team()? {
            Some(record) => self.view(&record).map(Some),
            None => Ok(None),
        }
    }

    /// Every peer call except `POST /v1/team/join`: maps the caller's WhoIs to a
    /// current member (see [`identity::authenticate`]).
    pub fn authenticate(&self, caller: Option<&WhoIs>) -> Result<Member> {
        identity::authenticate(&*self.store, caller)
    }

    /// `GET /v1/team`, served by every member's Mac; the creator's is authoritative.
    pub fn roster(&self) -> Result<RosterResponse> {
        let record = self.record()?;
        Ok(RosterResponse {
            members: self.store.members()?,
            version: record.roster_version,
        })
    }

    /// `host:port` of the creator's Mac, where a member's Mac re-fetches the
    /// roster (`GET /v1/team`) on reconnect.
    pub fn creator_address(&self) -> Result<Option<String>> {
        Ok(self
            .store
            .team()?
            .map(|record| link::address(&record.creator_host, record.creator_port)))
    }

    /// `team_create`: a new team with this Mac's user as creator, roster version 1.
    pub fn create(&self, display_name: Option<&str>) -> Result<Team> {
        if self.store.team()?.is_some() {
            return Err(conflict("this Mac is already in a team"));
        }
        // These may run a CLI, so they are asked before taking the lock.
        let me = self.identity.tailnet_self()?;
        let whois = identity::person(Some(&me.whois))?.clone();
        // Checked now, so every invitation link can be built later.
        let host = link::parse_host(&me.host)?;
        let github_login = identity::github_login(&self.identity.github_login()?)?;

        let _guard = self.lock();
        if self.store.team()?.is_some() {
            return Err(conflict("this Mac is already in a team"));
        }
        let now = self.now();
        let creator = Member {
            id: MemberId::generate(),
            display_name: identity::display_name(display_name.unwrap_or(""), &github_login),
            github_login,
            tailscale_login: whois.login,
            tailscale_node: whois.node,
            role: MemberRole::Creator,
            joined_at: now,
            online: true,
            last_seen_at: Some(now),
        };
        self.store.put_member(&creator)?;
        self.store.set_me(creator.id)?;
        let record = TeamRecord {
            id: TeamId::generate(),
            creator_host: host,
            creator_port: self.config.peer_port,
            roster_version: 0,
            open_invitation: None,
        };
        let record = self.commit_roster(record, vec![])?;
        self.changed(&record)
    }

    /// `team_invite`, creator only: a new single-use code and link, valid for
    /// [`TeamConfig::invitation_ttl`]. It replaces the open invitation, which stops
    /// working. The link carries this Mac's current tailnet host and peer port.
    pub fn invite(&self) -> Result<Invitation> {
        self.creator()?;
        // May run a CLI, so it is asked before taking the lock.
        let host = link::parse_host(&self.identity.tailnet_self()?.host)?;

        let _guard = self.lock();
        let (mut record, me) = self.creator()?;
        record.creator_host = host;
        record.creator_port = self.config.peer_port;
        let now = self.now();
        if let Some(open) = self.open_invitation(&record, now)? {
            self.store.put_invitation(&Invitation {
                expires_at: now,
                ..open
            })?;
        }
        let code = self.unused_code()?;
        let link =
            InvitationLink::new(&code, &record.creator_host, record.creator_port, record.id)?;
        let invitation = Invitation {
            id: InvitationId::generate(),
            code: code.clone(),
            created_by: me.id,
            expires_at: after(now, self.config.invitation_ttl),
            used_by: None,
            used_at: None,
            link: link.to_string(),
        };
        self.store.put_invitation(&invitation)?;
        record.open_invitation = Some(code);
        self.store.put_team(Some(&record))?;
        self.changed(&record)?;
        Ok(invitation)
    }

    /// `team_remove`, creator only. The removal is stored first, so the member's
    /// next peer call is refused, then the new roster goes out.
    pub fn remove(&self, member_id: MemberId) -> Result<Team> {
        let _guard = self.lock();
        let (record, me) = self.creator()?;
        if member_id == me.id {
            return Err(ApiError::new(
                ErrorCode::InvalidParams,
                "the creator cannot remove themselves",
            ));
        }
        if !self.store.members()?.iter().any(|m| m.id == member_id) {
            return Err(ApiError::new(ErrorCode::NotFound, "no such member"));
        }
        self.store.remove_member(member_id, self.now())?;
        let record = self.commit_roster(record, vec![member_id])?;
        self.changed(&record)
    }

    /// On a member's Mac: applies a roster from the creator's Mac (`from`, as
    /// authenticated by the peer server), pushed on `WS /v1/events` or fetched from
    /// `GET /v1/team` on reconnect. Older or equal versions change nothing
    /// (`Ok(None)`). Removals are applied first; when this Mac's own member is gone,
    /// it forgets the team and publishes an empty roster.
    pub fn apply_roster(
        &self,
        from: MemberId,
        roster: RosterResponse,
    ) -> Result<Option<RosterChange>> {
        let _guard = self.lock();
        let mut record = self.record()?;
        let me = self.me()?;
        let local = self.store.members()?;
        let creator = local.iter().find(|m| m.role == MemberRole::Creator);
        if creator.map(|c| c.id) != Some(from) {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "only the team creator's Mac sends the roster",
            ));
        }
        if from == me.id {
            return Err(conflict("this Mac holds the authoritative roster"));
        }
        if roster.version <= record.roster_version {
            return Ok(None);
        }
        if roster::check(&roster.members)?.id != from {
            return Err(ApiError::new(
                ErrorCode::InvalidParams,
                "the roster names another creator",
            ));
        }
        let keep: HashSet<MemberId> = roster.members.iter().map(|m| m.id).collect();
        let still_member = keep.contains(&me.id);
        let removed: Vec<MemberId> = local
            .iter()
            .map(|m| m.id)
            .filter(|id| !still_member || !keep.contains(id))
            .collect();
        let now = self.now();
        for id in &removed {
            self.store.remove_member(*id, now)?;
        }
        let change = if still_member {
            for member in &roster.members {
                self.store.put_member(member)?;
            }
            record.roster_version = roster.version;
            self.store.put_team(Some(&record))?;
            let team = self.changed(&record)?;
            RosterChange {
                version: roster.version,
                members: team.members,
                removed,
            }
        } else {
            self.store.put_team(None)?;
            self.events.publish(Event::TeamUpdate(TeamUpdateEvent {
                team: Team {
                    me: me.id,
                    members: vec![],
                    invitation: None,
                },
            }));
            RosterChange {
                version: roster.version,
                members: vec![],
                removed,
            }
        };
        // No receiver (no peer server running) is fine.
        let _ = self.roster_tx.send(change.clone());
        Ok(Some(change))
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    pub(super) fn now(&self) -> Timestamp {
        (self.clock)()
    }

    /// Locks without propagating poison: the state stays consistent because the
    /// store is the source of truth.
    pub(super) fn lock(&self) -> MutexGuard<'_, JoinLimiter> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn record(&self) -> Result<TeamRecord> {
        self.store
            .team()?
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "this Mac is not in a team"))
    }

    pub(super) fn me(&self) -> Result<Member> {
        let me = match self.store.me()? {
            Some(id) => self.store.member(id)?,
            None => None,
        };
        me.ok_or_else(|| ApiError::new(ErrorCode::Internal, "the team has no member for this Mac"))
    }

    /// The team and my member, when I created the team; `denied` otherwise.
    fn creator(&self) -> Result<(TeamRecord, Member)> {
        let record = self.record()?;
        let me = self.me()?;
        if me.role != MemberRole::Creator {
            return Err(ApiError::new(
                ErrorCode::Denied,
                "only the team creator can do this",
            ));
        }
        Ok((record, me))
    }

    pub(super) fn is_creator(&self) -> Result<bool> {
        Ok(self.store.team()?.is_some() && self.me()?.role == MemberRole::Creator)
    }

    /// Bumps the roster version, stores it, and sends the change to the peer server.
    pub(super) fn commit_roster(
        &self,
        mut record: TeamRecord,
        removed: Vec<MemberId>,
    ) -> Result<TeamRecord> {
        record.roster_version += 1;
        self.store.put_team(Some(&record))?;
        let change = RosterChange {
            version: record.roster_version,
            members: self.store.members()?,
            removed,
        };
        let _ = self.roster_tx.send(change);
        Ok(record)
    }

    /// Publishes `team.update` and returns the team.
    pub(super) fn changed(&self, record: &TeamRecord) -> Result<Team> {
        let team = self.view(record)?;
        self.events
            .publish(Event::TeamUpdate(TeamUpdateEvent { team: team.clone() }));
        Ok(team)
    }

    fn view(&self, record: &TeamRecord) -> Result<Team> {
        Ok(Team {
            me: self.me()?.id,
            members: self.store.members()?,
            invitation: self.open_invitation(record, self.now())?,
        })
    }

    fn open_invitation(&self, record: &TeamRecord, now: Timestamp) -> Result<Option<Invitation>> {
        let Some(code) = &record.open_invitation else {
            return Ok(None);
        };
        Ok(self
            .store
            .invitation_by_code(code)?
            .filter(|i| i.used_by.is_none() && i.expires_at > now))
    }

    fn unused_code(&self) -> Result<String> {
        for _ in 0..8 {
            let code = code::generate()?;
            if self.store.invitation_by_code(&code)?.is_none() {
                return Ok(code);
            }
        }
        Err(ApiError::new(
            ErrorCode::Internal,
            "could not pick an unused invitation code",
        ))
    }
}

pub(super) fn conflict(message: &str) -> ApiError {
    ApiError::new(ErrorCode::Conflict, message)
}

fn after(at: Timestamp, by: Duration) -> Timestamp {
    Timestamp::from(SystemTime::from(at) + by)
}
