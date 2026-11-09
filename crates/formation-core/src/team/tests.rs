//! The team against small doubles of its ports: an in-memory `TeamRepo`, a stub
//! local identity, an event sink that records, and a `Peers` that carries a join
//! from one `TeamService` (the invitee's Mac) to another (the creator's Mac) with a
//! scripted WhoIs. TASK-042's shared fakes replace these once merged.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::sync::broadcast;

use formation_proto::ask::Question;
use formation_proto::common::{
    ApiError, ErrorCode, MemberId, QuestionId, RequestId, Seq, Timestamp,
};
use formation_proto::events::Event;
use formation_proto::peer::{
    PeerAsk, PeerAskAccepted, PeerEvent, PeerJoin, PeerJoinAccepted, PeerQuestionAction,
    PeerRequestCreate, RosterResponse, SharedAgentView,
};
use formation_proto::policy::{Caller, Chain};
use formation_proto::request::WorkRequest;
use formation_proto::team::{Invitation, Member, MemberRole, Team, TeamId, TeamJoin, WhoIs};

use super::*;
use crate::ports::{
    CommandCall, CommandHandler, EventSink, MIGRATIONS_B, Peers, Result, TeamRecord, TeamRepo,
};

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

/// `TeamRepo` in memory, keeping the port's rules and the uniqueness the SQL
/// schema has (logins among current members, invitation codes).
#[derive(Default)]
struct MemoryTeam {
    state: Mutex<TeamState>,
}

#[derive(Default)]
struct TeamState {
    me: Option<MemberId>,
    team: Option<TeamRecord>,
    members: Vec<Member>,
    removed: HashMap<MemberId, Timestamp>,
    invitations: Vec<Invitation>,
}

impl MemoryTeam {
    fn invitation(&self, code: &str) -> Invitation {
        self.invitation_by_code(code).unwrap().expect("invitation")
    }

    fn is_removed(&self, id: MemberId) -> bool {
        self.state.lock().unwrap().removed.contains_key(&id)
    }
}

impl TeamRepo for MemoryTeam {
    fn me(&self) -> Result<Option<MemberId>> {
        Ok(self.state.lock().unwrap().me)
    }

    fn set_me(&self, id: MemberId) -> Result<()> {
        self.state.lock().unwrap().me = Some(id);
        Ok(())
    }

    fn team(&self) -> Result<Option<TeamRecord>> {
        Ok(self.state.lock().unwrap().team.clone())
    }

    fn put_team(&self, team: Option<&TeamRecord>) -> Result<()> {
        self.state.lock().unwrap().team = team.cloned();
        Ok(())
    }

    fn members(&self) -> Result<Vec<Member>> {
        let state = self.state.lock().unwrap();
        let mut members: Vec<Member> = state
            .members
            .iter()
            .filter(|m| !state.removed.contains_key(&m.id))
            .cloned()
            .collect();
        members.sort_by_key(|m| m.joined_at);
        Ok(members)
    }

    fn member(&self, id: MemberId) -> Result<Option<Member>> {
        let state = self.state.lock().unwrap();
        Ok(state.members.iter().find(|m| m.id == id).cloned())
    }

    fn member_by_tailscale(&self, login: &str) -> Result<Option<Member>> {
        Ok(self
            .members()?
            .into_iter()
            .find(|m| m.tailscale_login == login))
    }

    fn put_member(&self, member: &Member) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let clash = state.members.iter().any(|m| {
            m.id != member.id
                && !state.removed.contains_key(&m.id)
                && (m.github_login.eq_ignore_ascii_case(&member.github_login)
                    || m.tailscale_login == member.tailscale_login)
        });
        if clash {
            return Err(ApiError::new(ErrorCode::Conflict, "login taken"));
        }
        state.members.retain(|m| m.id != member.id);
        state.members.push(member.clone());
        state.removed.remove(&member.id);
        Ok(())
    }

    fn remove_member(&self, id: MemberId, at: Timestamp) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if !state.members.iter().any(|m| m.id == id) {
            return Err(ApiError::new(ErrorCode::NotFound, "no member"));
        }
        state.removed.entry(id).or_insert(at);
        Ok(())
    }

    fn put_invitation(&self, invitation: &Invitation) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state
            .invitations
            .iter()
            .any(|i| i.id != invitation.id && i.code == invitation.code)
        {
            return Err(ApiError::new(ErrorCode::Conflict, "code taken"));
        }
        state.invitations.retain(|i| i.id != invitation.id);
        state.invitations.push(invitation.clone());
        Ok(())
    }

    fn invitation_by_code(&self, code: &str) -> Result<Option<Invitation>> {
        let state = self.state.lock().unwrap();
        Ok(state.invitations.iter().find(|i| i.code == code).cloned())
    }
}

struct StubIdentity {
    tailnet: Mutex<TailnetSelf>,
    github_login: String,
}

impl LocalIdentity for StubIdentity {
    fn tailnet_self(&self) -> Result<TailnetSelf> {
        Ok(self.tailnet.lock().unwrap().clone())
    }

    fn github_login(&self) -> Result<String> {
        Ok(self.github_login.clone())
    }
}

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<Event>>,
}

impl RecordingSink {
    fn teams(&self) -> Vec<Team> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Event::TeamUpdate(update) => Some(update.team.clone()),
                _ => None,
            })
            .collect()
    }
}

impl EventSink for RecordingSink {
    fn publish(&self, event: Event) -> Seq {
        let mut events = self.events.lock().unwrap();
        events.push(event);
        Seq(events.len() as u64)
    }
}

type Tamper = Box<dyn Fn(&mut PeerJoinAccepted) + Send>;

/// The invitee's `Peers`: `join` reaches the creator's `TeamService` as the peer
/// server would, with the WhoIs Tailscale reports for this Mac.
struct LoopbackPeers {
    creator: Option<Arc<TeamService<MemoryTeam>>>,
    whois: Option<WhoIs>,
    offline: AtomicBool,
    /// Rewrites the creator's reply, to test what the invitee checks.
    tamper: Mutex<Option<Tamper>>,
    addresses: Mutex<Vec<String>>,
    sent: Mutex<Vec<PeerJoin>>,
}

impl LoopbackPeers {
    fn to(creator: Option<Arc<TeamService<MemoryTeam>>>, whois: Option<WhoIs>) -> Self {
        Self {
            creator,
            whois,
            offline: AtomicBool::new(false),
            tamper: Mutex::new(None),
            addresses: Mutex::default(),
            sent: Mutex::default(),
        }
    }
}

fn unused() -> ApiError {
    ApiError::new(ErrorCode::Internal, "not used by the team")
}

#[async_trait]
impl Peers for LoopbackPeers {
    async fn join(&self, creator_address: &str, join: PeerJoin) -> Result<PeerJoinAccepted> {
        self.addresses
            .lock()
            .unwrap()
            .push(creator_address.to_owned());
        self.sent.lock().unwrap().push(join.clone());
        if self.offline.load(Ordering::SeqCst) {
            return Err(ApiError::new(ErrorCode::PeerOffline, "creator offline"));
        }
        let creator = self.creator.as_ref().ok_or_else(unused)?;
        let mut accepted = creator.handle_join(self.whois.as_ref(), &join)?;
        if let Some(tamper) = self.tamper.lock().unwrap().as_ref() {
            tamper(&mut accepted);
        }
        Ok(accepted)
    }

    async fn roster(&self, _: MemberId) -> Result<Vec<Member>> {
        Err(unused())
    }

    async fn agents(&self, _: MemberId) -> Result<Vec<SharedAgentView>> {
        Err(unused())
    }

    async fn ask(&self, _: MemberId, _: PeerAsk) -> Result<PeerAskAccepted> {
        Err(unused())
    }

    async fn question_action(
        &self,
        _: MemberId,
        _: QuestionId,
        _: PeerQuestionAction,
    ) -> Result<Question> {
        Err(unused())
    }

    async fn create_request(&self, _: MemberId, _: PeerRequestCreate) -> Result<WorkRequest> {
        Err(unused())
    }

    async fn request(&self, _: MemberId, _: RequestId) -> Result<WorkRequest> {
        Err(unused())
    }

    async fn cancel_request(&self, _: MemberId, _: RequestId) -> Result<WorkRequest> {
        Err(unused())
    }

    fn subscribe(&self) -> broadcast::Receiver<PeerEvent> {
        broadcast::channel(1).1
    }
}

#[derive(Clone)]
struct TestClock(Arc<Mutex<Timestamp>>);

impl TestClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(
            "2026-11-09T09:00:00Z".parse().unwrap(),
        )))
    }

    fn clock(&self) -> Clock {
        let now = self.0.clone();
        Arc::new(move || *now.lock().unwrap())
    }

    fn now(&self) -> Timestamp {
        *self.0.lock().unwrap()
    }

    fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap();
        *now = Timestamp::from(SystemTime::from(*now) + by);
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const CREATOR_HOST: &str = "dohyeon-mbp.tail0000.ts.net";

fn whois(name: &str) -> WhoIs {
    WhoIs {
        login: format!("{name}@example.com"),
        node: format!("n{}", name.to_uppercase()),
    }
}

/// One Mac running formationd with the team module.
struct Mac {
    store: Arc<MemoryTeam>,
    events: Arc<RecordingSink>,
    peers: Arc<LoopbackPeers>,
    identity: Arc<StubIdentity>,
    team: Arc<TeamService<MemoryTeam>>,
}

impl Mac {
    fn new(clock: &TestClock, name: &str, peers: LoopbackPeers) -> Self {
        let store = Arc::new(MemoryTeam::default());
        let events = Arc::new(RecordingSink::default());
        let peers = Arc::new(peers);
        let identity = Arc::new(StubIdentity {
            tailnet: Mutex::new(TailnetSelf {
                whois: whois(name),
                host: format!("{name}-mbp.tail0000.ts.net"),
            }),
            github_login: name.to_owned(),
        });
        let team = Arc::new(TeamService::new(
            store.clone(),
            identity.clone(),
            peers.clone(),
            events.clone(),
            clock.clock(),
            TeamConfig::default(),
        ));
        Self {
            store,
            events,
            peers,
            identity,
            team,
        }
    }

    fn me(&self) -> MemberId {
        self.store.me().unwrap().unwrap()
    }

    fn version(&self) -> u64 {
        self.store.team().unwrap().unwrap().roster_version
    }

    fn join(&self, link: &str) -> Result<Team> {
        block_on(self.team.join(&TeamJoin {
            code: link.to_owned(),
            display_name: None,
        }))
    }
}

/// Dohyeon's Mac with a team, and invitees' Macs that reach it.
struct World {
    clock: TestClock,
    creator: Mac,
}

impl World {
    fn new() -> Self {
        let clock = TestClock::new();
        let creator = Mac::new(&clock, "dohyeon", LoopbackPeers::to(None, None));
        creator.team.create(Some("Dohyeon")).unwrap();
        Self { clock, creator }
    }

    /// A Mac whose connections Tailscale reports as `name`'s.
    fn invitee(&self, name: &str) -> Mac {
        self.invitee_as(name, Some(whois(name)))
    }

    fn invitee_as(&self, name: &str, caller: Option<WhoIs>) -> Mac {
        let peers = LoopbackPeers::to(Some(self.creator.team.clone()), caller);
        Mac::new(&self.clock, name, peers)
    }

    fn invite(&self) -> Invitation {
        self.creator.team.invite().unwrap()
    }

    /// A direct `POST /v1/team/join` on the creator's Mac.
    fn post_join(
        &self,
        caller: Option<&WhoIs>,
        code: &str,
        github: &str,
    ) -> Result<PeerJoinAccepted> {
        self.creator.team.handle_join(
            caller,
            &PeerJoin {
                code: code.to_owned(),
                github_login: github.to_owned(),
                display_name: github.to_owned(),
            },
        )
    }

    fn joined(&self, name: &str) -> Mac {
        let mac = self.invitee(name);
        mac.join(&self.invite().link).unwrap();
        mac
    }

    /// Sends the creator's current roster to a member's Mac, as the push would.
    fn push_roster(&self, to: &Mac) -> Result<Option<RosterChange>> {
        to.team
            .apply_roster(self.creator.me(), self.creator.team.roster().unwrap())
    }
}

fn code_of(error: ApiError) -> ErrorCode {
    error.code
}

// ---------------------------------------------------------------------------
// Creating and inviting
// ---------------------------------------------------------------------------

#[test]
fn creating_a_team_makes_me_its_creator_at_roster_version_one() {
    let world = World::new();
    let mac = &world.creator;
    let team = mac.team.team().unwrap().expect("team");
    assert_eq!(team.members.len(), 1);
    let me = &team.members[0];
    assert_eq!(team.me, me.id);
    assert_eq!(me.role, MemberRole::Creator);
    assert_eq!(me.display_name, "Dohyeon");
    assert_eq!(me.github_login, "dohyeon");
    assert_eq!(
        (me.tailscale_login.as_str(), me.tailscale_node.as_str()),
        ("dohyeon@example.com", "nDOHYEON")
    );
    assert_eq!(team.invitation, None);
    assert_eq!(mac.version(), 1);
    let record = mac.store.team().unwrap().unwrap();
    assert_eq!(
        (record.creator_host.as_str(), record.creator_port),
        (CREATOR_HOST, 7413)
    );
    assert_eq!(mac.events.teams(), vec![team]);

    let again = mac.team.create(None).unwrap_err();
    assert_eq!(again.code, ErrorCode::Conflict);
}

#[test]
fn a_tagged_mac_cannot_create_a_team() {
    let clock = TestClock::new();
    let mut mac = Mac::new(&clock, "server", LoopbackPeers::to(None, None));
    let identity = Arc::new(StubIdentity {
        tailnet: Mutex::new(TailnetSelf {
            whois: WhoIs {
                login: identity::TAGGED_DEVICES_LOGIN.into(),
                node: "nSERVER".into(),
            },
            host: "server.tail0000.ts.net".into(),
        }),
        github_login: "server".into(),
    });
    mac.team = Arc::new(TeamService::new(
        mac.store.clone(),
        identity,
        mac.peers.clone(),
        mac.events.clone(),
        clock.clock(),
        TeamConfig::default(),
    ));
    assert_eq!(
        code_of(mac.team.create(None).unwrap_err()),
        ErrorCode::NotMember
    );
    assert_eq!(mac.team.team().unwrap(), None);
}

#[test]
fn an_invitation_is_a_link_to_the_creators_mac_that_expires_in_a_day() {
    let world = World::new();
    let invitation = world.invite();
    assert_eq!(
        code::normalize(&invitation.code),
        Some(invitation.code.clone())
    );
    let link = InvitationLink::parse(&invitation.link).unwrap();
    let record = world.creator.store.team().unwrap().unwrap();
    assert_eq!(link.code, invitation.code);
    assert_eq!(
        (link.host.as_str(), link.port, link.team),
        (CREATOR_HOST, 7413, record.id)
    );
    assert_eq!(
        limit::elapsed(world.clock.now(), invitation.expires_at),
        Duration::from_secs(24 * 60 * 60)
    );
    assert_eq!(invitation.created_by, world.creator.me());
    assert_eq!((invitation.used_by, invitation.used_at), (None, None));
    let team = world.creator.team.team().unwrap().unwrap();
    assert_eq!(team.invitation, Some(invitation.clone()));
    assert_eq!(world.creator.events.teams().last(), Some(&team));
}

#[test]
fn an_invitation_carries_the_creators_current_address() {
    let world = World::new();
    world.creator.identity.tailnet.lock().unwrap().host = "100.66.100.62".into();
    let invitation = world.invite();
    let link = InvitationLink::parse(&invitation.link).unwrap();
    assert_eq!(link.address(), "100.66.100.62:7413");
    let record = world.creator.store.team().unwrap().unwrap();
    assert_eq!(record.creator_host, "100.66.100.62");
    world.invitee("minsu").join(&invitation.link).unwrap();
}

#[test]
fn a_new_invitation_replaces_the_open_one() {
    let world = World::new();
    let first = world.invite();
    let second = world.invite();
    assert_ne!(first.code, second.code);
    assert_eq!(
        world.creator.store.invitation(&first.code).expires_at,
        world.clock.now()
    );
    let error = world.invitee("minsu").join(&first.link).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains("expired"), "{error}");
    world.invitee("minsu").join(&second.link).unwrap();
}

#[test]
fn only_the_creator_invites_and_removes() {
    let world = World::new();
    let minsu = world.joined("minsu");
    assert_eq!(code_of(minsu.team.invite().unwrap_err()), ErrorCode::Denied);
    assert_eq!(
        code_of(minsu.team.remove(world.creator.me()).unwrap_err()),
        ErrorCode::Denied
    );
    let clock = TestClock::new();
    let lonely = Mac::new(&clock, "lonely", LoopbackPeers::to(None, None));
    assert_eq!(
        code_of(lonely.team.invite().unwrap_err()),
        ErrorCode::NotFound
    );
}

// ---------------------------------------------------------------------------
// Joining
// ---------------------------------------------------------------------------

#[test]
fn joining_records_the_member_from_whois_and_returns_the_roster() {
    let world = World::new();
    let mut roster_changes = world.creator.team.subscribe_roster();
    let invitation = world.invite();
    let minsu = world.invitee("minsu");

    let team = minsu.join(&invitation.link).unwrap();

    // The invitee's Mac sent the code, its GitHub login, and a display name to the
    // creator's address from the link.
    assert_eq!(
        *minsu.peers.addresses.lock().unwrap(),
        vec![format!("{CREATOR_HOST}:7413")]
    );
    let sent = minsu.peers.sent.lock().unwrap()[0].clone();
    assert_eq!(
        (
            sent.code.as_str(),
            sent.github_login.as_str(),
            sent.display_name.as_str()
        ),
        (invitation.code.as_str(), "minsu", "minsu")
    );

    // The creator recorded the Tailscale login and node from WhoIs.
    let creator_view = world.creator.team.team().unwrap().unwrap();
    assert_eq!(creator_view.members.len(), 2);
    let member = &creator_view.members[1];
    assert_eq!(member.role, MemberRole::Member);
    assert_eq!(member.github_login, "minsu");
    assert_eq!(
        (
            member.tailscale_login.as_str(),
            member.tailscale_node.as_str()
        ),
        ("minsu@example.com", "nMINSU")
    );
    assert_eq!(member.joined_at, world.clock.now());
    let used = world.creator.store.invitation(&invitation.code);
    assert_eq!(
        (used.used_by, used.used_at),
        (Some(member.id), Some(world.clock.now()))
    );
    assert_eq!(
        creator_view.invitation, None,
        "a used invitation is no longer open"
    );
    assert_eq!(world.creator.version(), 2);
    let change = roster_changes.try_recv().unwrap();
    assert_eq!((change.version, change.removed.clone()), (2, vec![]));
    assert_eq!(change.members, creator_view.members);
    assert_eq!(
        change.to_peer_event(),
        PeerEvent::Roster {
            members: creator_view.members.clone(),
            version: 2
        }
    );

    // The invitee's Mac now holds the same roster, the team, and the creator's address.
    assert_eq!(team.me, member.id);
    assert_eq!(team.members, creator_view.members);
    assert_eq!(team.invitation, None);
    assert_eq!(minsu.events.teams(), vec![team]);
    let record = minsu.store.team().unwrap().unwrap();
    assert_eq!(record.id, world.creator.store.team().unwrap().unwrap().id);
    assert_eq!(record.roster_version, 2);
    assert_eq!(
        minsu.team.creator_address().unwrap().as_deref(),
        Some("dohyeon-mbp.tail0000.ts.net:7413")
    );
    assert_eq!(
        minsu.team.roster().unwrap(),
        world.creator.team.roster().unwrap()
    );
}

#[test]
fn the_body_cannot_choose_the_tailscale_identity() {
    // PeerJoin has no Tailscale fields at all; the member gets the connection's WhoIs.
    let world = World::new();
    let invitation = world.invite();
    let accepted = world
        .post_join(
            Some(&whois("minsu")),
            &invitation.code.to_lowercase(),
            "minsu",
        )
        .unwrap();
    let member = world
        .creator
        .store
        .member(accepted.member_id)
        .unwrap()
        .unwrap();
    assert_eq!(member.tailscale_login, "minsu@example.com");
    assert_eq!(
        accepted.team_id,
        Some(world.creator.store.team().unwrap().unwrap().id)
    );
    assert_eq!(accepted.version, 2);
}

#[test]
fn an_invitation_works_until_it_expires_after_24_hours() {
    let world = World::new();
    let invitation = world.invite();
    world.clock.advance(Duration::from_secs(24 * 60 * 60 - 1));
    world.invitee("minsu").join(&invitation.link).unwrap();

    let late = world.invite();
    world.clock.advance(Duration::from_secs(24 * 60 * 60));
    let jiwoo = world.invitee("jiwoo");
    let error = jiwoo.join(&late.link).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains("expired"), "{error}");
    assert_eq!(
        jiwoo.store.team().unwrap(),
        None,
        "nothing stored on refusal"
    );
    assert_eq!(world.creator.store.invitation(&late.code).used_by, None);
    assert_eq!(world.creator.team.team().unwrap().unwrap().invitation, None);
}

#[test]
fn an_invitation_works_once() {
    let world = World::new();
    let invitation = world.invite();
    let minsu = world.invitee("minsu");
    let first = minsu.join(&invitation.link).unwrap();

    let error = world.invitee("jiwoo").join(&invitation.link).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.contains("already used"), "{error}");

    // The joiner's own retry (say the reply was lost) gets the same answer.
    let retry = world
        .post_join(Some(&whois("minsu")), &invitation.code, "minsu")
        .unwrap();
    assert_eq!(retry.member_id, first.me);
    assert_eq!(retry.version, 2);
    assert_eq!(world.creator.team.roster().unwrap().members.len(), 2);
}

#[test]
fn bad_codes_are_rate_limited_per_caller() {
    let world = World::new();
    let invitation = world.invite();
    let eve = whois("eve");
    let per_caller = TeamConfig::default().join_limit.per_caller;
    for i in 0..per_caller {
        let error = world
            .post_join(Some(&eve), &format!("0000-000{i}"), "eve")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
    }
    // Locked out: even the right code is refused, before it is looked at.
    let error = world
        .post_join(Some(&eve), &invitation.code, "eve")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RateLimited);
    assert_eq!(
        world.creator.store.invitation(&invitation.code).used_by,
        None
    );

    // Someone else is not affected.
    world
        .post_join(Some(&whois("minsu")), &invitation.code, "minsu")
        .unwrap();

    // After the window, eve may try again.
    let next = world.invite();
    world.clock.advance(TeamConfig::default().join_limit.window);
    world.post_join(Some(&eve), &next.code, "eve").unwrap();
}

#[test]
fn bad_codes_from_everyone_together_are_rate_limited() {
    let world = World::new();
    let invitation = world.invite();
    let limit = TeamConfig::default().join_limit;
    for i in 0..limit.total {
        let caller = whois(&format!("guesser{i}"));
        world
            .post_join(Some(&caller), "garbage", "guesser")
            .unwrap_err();
    }
    let error = world
        .post_join(Some(&whois("minsu")), &invitation.code, "minsu")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::RateLimited);
}

#[test]
fn tagged_and_unknown_nodes_cannot_join_or_count_as_attempts() {
    let world = World::new();
    let invitation = world.invite();
    let tagged = WhoIs {
        login: identity::TAGGED_DEVICES_LOGIN.into(),
        node: "nSERVER".into(),
    };
    for caller in [None, Some(&tagged)] {
        let error = world
            .post_join(caller, &invitation.code, "server")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotMember);
    }
    let error = world
        .invitee_as("server", None)
        .join(&invitation.link)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotMember);
    assert_eq!(
        world.creator.store.invitation(&invitation.code).used_by,
        None
    );
}

#[test]
fn a_member_cannot_join_again_and_does_not_use_up_the_code() {
    let world = World::new();
    world.joined("minsu");
    let invitation = world.invite();

    // Same Tailscale account from another Mac (node).
    let other_node = WhoIs {
        login: "minsu@example.com".into(),
        node: "nMINSU-MINI".into(),
    };
    let error = world
        .post_join(Some(&other_node), &invitation.code, "minsu")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    // The creator's own Tailscale account.
    let error = world
        .post_join(Some(&whois("dohyeon")), &invitation.code, "dohyeon")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    // A GitHub login already in the team, ignoring case.
    let error = world
        .post_join(Some(&whois("jiwoo")), &invitation.code, "MINSU")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);

    assert_eq!(
        world.creator.store.invitation(&invitation.code).used_by,
        None
    );
    world
        .post_join(Some(&whois("jiwoo")), &invitation.code, "jiwoo")
        .unwrap();
}

#[test]
fn a_malformed_github_login_is_invalid_params() {
    let world = World::new();
    let invitation = world.invite();
    let error = world
        .post_join(Some(&whois("minsu")), &invitation.code, "minsu kim")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);
}

#[test]
fn only_the_creators_mac_takes_joins() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let error = minsu
        .team
        .handle_join(
            Some(&whois("jiwoo")),
            &PeerJoin {
                code: "7F3K-Q9PX".into(),
                github_login: "jiwoo".into(),
                display_name: "Jiwoo".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let clock = TestClock::new();
    let no_team = Mac::new(&clock, "x", LoopbackPeers::to(None, None));
    let error = no_team
        .team
        .handle_join(
            Some(&whois("jiwoo")),
            &PeerJoin {
                code: "7F3K-Q9PX".into(),
                github_login: "jiwoo".into(),
                display_name: "Jiwoo".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn the_invitee_refuses_bad_links_and_bad_replies() {
    let world = World::new();
    let invitation = world.invite();
    let minsu = world.invitee("minsu");

    for bad in [invitation.code.as_str(), "formation://join/7F3K-Q9PX"] {
        assert_eq!(
            code_of(minsu.join(bad).unwrap_err()),
            ErrorCode::InvalidParams
        );
    }
    assert!(minsu.peers.sent.lock().unwrap().is_empty());

    minsu.peers.offline.store(true, Ordering::SeqCst);
    assert_eq!(
        code_of(minsu.join(&invitation.link).unwrap_err()),
        ErrorCode::PeerOffline
    );
    minsu.peers.offline.store(false, Ordering::SeqCst);

    *minsu.peers.tamper.lock().unwrap() = Some(Box::new(|accepted| {
        accepted.team_id = Some(TeamId::generate())
    }));
    let error = minsu.join(&invitation.link).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(error.message.contains("another team"), "{error}");
    assert_eq!(minsu.store.team().unwrap(), None);
    assert!(minsu.events.teams().is_empty());
}

#[test]
fn a_mac_in_a_team_cannot_join_another() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let error = minsu.join(&world.invite().link).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(
        code_of(world.creator.join(&world.invite().link).unwrap_err()),
        ErrorCode::Conflict
    );
}

// ---------------------------------------------------------------------------
// WhoIs to member
// ---------------------------------------------------------------------------

#[test]
fn whois_maps_to_a_member_by_login_and_node() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let creator = &world.creator.team;

    assert_eq!(
        creator.authenticate(Some(&whois("minsu"))).unwrap().id,
        minsu.me()
    );
    // Members' Macs map the creator the same way, from their copy of the roster.
    assert_eq!(
        minsu.team.authenticate(Some(&whois("dohyeon"))).unwrap().id,
        world.creator.me()
    );

    let refused = |caller: Option<&WhoIs>| code_of(creator.authenticate(caller).unwrap_err());
    let other_node = WhoIs {
        login: "minsu@example.com".into(),
        node: "nPHONE".into(),
    };
    let tagged = WhoIs {
        login: identity::TAGGED_DEVICES_LOGIN.into(),
        node: "nMINSU".into(),
    };
    assert_eq!(refused(Some(&other_node)), ErrorCode::NotMember);
    assert_eq!(refused(Some(&whois("stranger"))), ErrorCode::NotMember);
    assert_eq!(refused(Some(&tagged)), ErrorCode::NotMember);
    assert_eq!(refused(None), ErrorCode::NotMember);
}

// ---------------------------------------------------------------------------
// Roster versions, distribution, and removal
// ---------------------------------------------------------------------------

#[test]
fn removal_ends_access_at_once_and_bumps_the_version() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let mut changes = world.creator.team.subscribe_roster();

    let team = world.creator.team.remove(minsu.me()).unwrap();

    assert_eq!(
        code_of(
            world
                .creator
                .team
                .authenticate(Some(&whois("minsu")))
                .unwrap_err()
        ),
        ErrorCode::NotMember
    );
    assert_eq!(team.members.len(), 1);
    assert_eq!(world.creator.version(), 3);
    assert!(world.creator.store.is_removed(minsu.me()));
    let change = changes.try_recv().unwrap();
    assert_eq!((change.version, change.removed), (3, vec![minsu.me()]));
    assert_eq!(world.creator.events.teams().last(), Some(&team));

    // The removed member keeps its row for history, and the code it used stays used.
    assert!(world.creator.store.member(minsu.me()).unwrap().is_some());

    // Removing again, the creator, or a stranger.
    let creator = &world.creator.team;
    assert_eq!(
        code_of(creator.remove(minsu.me()).unwrap_err()),
        ErrorCode::NotFound
    );
    assert_eq!(
        code_of(creator.remove(world.creator.me()).unwrap_err()),
        ErrorCode::InvalidParams
    );
    assert_eq!(
        code_of(creator.remove(MemberId::generate()).unwrap_err()),
        ErrorCode::NotFound
    );
    assert_eq!(world.creator.version(), 3);
}

#[test]
fn a_removed_member_can_join_again_with_a_new_invitation() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let old = minsu.me();
    world.creator.team.remove(old).unwrap();
    let again = world
        .post_join(Some(&whois("minsu")), &world.invite().code, "minsu")
        .unwrap();
    assert_ne!(again.member_id, old);
    assert_eq!(world.creator.version(), 4);
}

#[test]
fn members_apply_newer_rosters_only() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let stale = world.creator.team.roster().unwrap();
    world.joined("jiwoo");

    let change = world.push_roster(&minsu).unwrap().expect("applied");
    assert_eq!(change.version, 3);
    assert_eq!(change.removed, vec![]);
    assert_eq!(minsu.version(), 3);
    assert_eq!(
        minsu.team.roster().unwrap(),
        world.creator.team.roster().unwrap()
    );
    assert_eq!(minsu.events.teams().last().unwrap().members.len(), 3);

    // The same push again, or an older roster (a slow fetch), changes nothing.
    assert_eq!(world.push_roster(&minsu).unwrap(), None);
    assert_eq!(
        minsu.team.apply_roster(world.creator.me(), stale).unwrap(),
        None
    );
    assert_eq!(minsu.team.roster().unwrap().members.len(), 3);
}

#[test]
fn members_apply_removals_first() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let jiwoo = world.joined("jiwoo");
    world.push_roster(&minsu).unwrap();
    let mut changes = minsu.team.subscribe_roster();

    world.creator.team.remove(jiwoo.me()).unwrap();
    let change = world.push_roster(&minsu).unwrap().expect("applied");

    assert_eq!(change.removed, vec![jiwoo.me()]);
    assert_eq!(changes.try_recv().unwrap(), change);
    assert!(minsu.store.is_removed(jiwoo.me()));
    assert_eq!(
        code_of(minsu.team.authenticate(Some(&whois("jiwoo"))).unwrap_err()),
        ErrorCode::NotMember
    );
    assert_eq!(minsu.version(), world.creator.version());
}

#[test]
fn a_removed_members_mac_forgets_the_team() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let me = minsu.me();
    world.creator.team.remove(me).unwrap();

    let change = world.push_roster(&minsu).unwrap().expect("applied");

    assert_eq!(change.members, vec![]);
    assert!(change.removed.contains(&me));
    assert!(change.removed.contains(&world.creator.me()));
    assert_eq!(minsu.team.team().unwrap(), None);
    assert!(minsu.store.members().unwrap().is_empty());
    let last = minsu.events.teams().pop().unwrap();
    assert_eq!((last.me, last.members, last.invitation), (me, vec![], None));
    // And it can join a team again, with the creator (removed locally when the
    // team was forgotten) a current member once more.
    let team = minsu.join(&world.invite().link).unwrap();
    assert_eq!(team.members.len(), 2);
    assert!(!minsu.store.is_removed(world.creator.me()));
    assert_eq!(
        minsu.team.roster().unwrap(),
        world.creator.team.roster().unwrap()
    );
    world.joined("jiwoo");
    assert!(world.push_roster(&minsu).unwrap().is_some());
}

#[test]
fn only_the_creators_mac_sends_rosters_and_they_must_be_consistent() {
    let world = World::new();
    let minsu = world.joined("minsu");
    let jiwoo = world.joined("jiwoo");
    world.push_roster(&minsu).unwrap();
    let mut roster = world.creator.team.roster().unwrap();
    roster.version += 1;

    let error = minsu
        .team
        .apply_roster(jiwoo.me(), roster.clone())
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);

    let mut two_creators = roster.clone();
    two_creators.members[1].role = MemberRole::Creator;
    let error = minsu
        .team
        .apply_roster(world.creator.me(), two_creators)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);

    let mut twice = roster.clone();
    twice.members.push(twice.members[1].clone());
    let error = minsu
        .team
        .apply_roster(world.creator.me(), twice)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);

    let error = world
        .creator
        .team
        .apply_roster(world.creator.me(), roster)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(minsu.version(), 3, "nothing applied");
}

#[test]
fn roster_versions_count_every_change_from_one() {
    let world = World::new();
    assert_eq!(world.creator.team.roster().unwrap().version, 1);
    let minsu = world.joined("minsu");
    assert_eq!(world.creator.team.roster().unwrap().version, 2);
    world.invite();
    assert_eq!(
        world.creator.team.roster().unwrap().version,
        2,
        "invitations are not the roster"
    );
    world.creator.team.remove(minsu.me()).unwrap();
    assert_eq!(world.creator.team.roster().unwrap().version, 3);
    let response: RosterResponse = world.creator.team.roster().unwrap();
    assert_eq!(response.members.len(), 1);
}

// ---------------------------------------------------------------------------
// Commands and migrations
// ---------------------------------------------------------------------------

#[test]
fn the_team_commands_go_through_the_command_handler() {
    let clock = TestClock::new();
    let mac = Mac::new(&clock, "dohyeon", LoopbackPeers::to(None, None));
    let call = |method: &str, params: serde_json::Value| CommandCall {
        caller: Caller::Owner,
        method: method.into(),
        params,
    };
    let handler: &dyn CommandHandler = &*mac.team;
    assert_eq!(
        handler.methods(),
        vec!["team_create", "team_invite", "team_join", "team_remove"]
    );

    let team = block_on(handler.handle(call("team_create", serde_json::Value::Null))).unwrap();
    let team: Team = serde_json::from_value(team).unwrap();
    assert_eq!(team.members[0].display_name, "dohyeon");

    let invitation = block_on(handler.handle(call("team_invite", serde_json::json!({})))).unwrap();
    let invitation: Invitation = serde_json::from_value(invitation).unwrap();
    assert!(invitation.link.starts_with("formation://join/"));

    let error = block_on(handler.handle(call("team_remove", serde_json::json!({})))).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidParams);

    let error = block_on(handler.handle(CommandCall {
        caller: Caller::Member {
            member_id: MemberId::generate(),
            via_agent: None,
            chain: Chain {
                id: formation_proto::common::ChainId::generate(),
                depth: 0,
            },
        },
        ..call("team_invite", serde_json::Value::Null)
    }))
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn joining_through_the_command_handler() {
    let world = World::new();
    let minsu = world.invitee("minsu");
    let params = serde_json::json!({ "code": world.invite().link, "display_name": "Minsu Park" });
    let team = block_on(minsu.team.handle(CommandCall {
        caller: Caller::Owner,
        method: "team_join".into(),
        params,
    }))
    .unwrap();
    let team: Team = serde_json::from_value(team).unwrap();
    assert_eq!(team.members[1].display_name, "Minsu Park");
}

#[test]
fn migrations_are_in_bs_range_and_in_order() {
    assert!(!MIGRATIONS.is_empty());
    for pair in MIGRATIONS.windows(2) {
        assert!(pair[0].version < pair[1].version);
    }
    for migration in MIGRATIONS {
        assert!(
            MIGRATIONS_B.contains(&migration.version),
            "{}",
            migration.version
        );
        // The runner owns transactions and connection settings.
        let statements: String = migration
            .sql
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_uppercase();
        for word in ["BEGIN", "COMMIT", "PRAGMA"] {
            assert!(!statements.contains(word), "{word}");
        }
    }
    let sql = MIGRATIONS[0].sql;
    for table in [
        "CREATE TABLE member",
        "CREATE TABLE invitation",
        "CREATE TABLE team",
    ] {
        assert!(sql.contains(table), "{table}");
    }
}
