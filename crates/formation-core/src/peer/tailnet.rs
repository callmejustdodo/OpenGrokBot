//! The real tailnet adapters behind the peer protocol (#179): who is calling
//! this Mac's peer server, and where each teammate's peer server is.
//!
//! - [`TailscaleCallers`] is the server's [`CallerIdentity`]: Tailscale WhoIs on
//!   the connection's TCP peer address, run off the async runtime.
//! - [`TailnetDirectory`] is the client's [`PeerDirectory`]: the team's roster
//!   from the [`TeamRepo`], and each member's address from their Mac's node in
//!   Tailscale's peer list.
//!
//! Both take the one [`Tailscale`] formationd discovers at start, so its
//! LocalAPI endpoint is found (and rediscovered) once for every lookup.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use formation_proto::common::MemberId;
use formation_proto::peer::PEER_PORT;
use formation_proto::team::{Member, MemberRole, WhoIs};

use super::api::{BlockingIdentity, CallerIdentity, not_member};
use super::client::PeerDirectory;
use super::identity::{WhoIsError, WhoIsResolver};
use super::server::is_tailscale_ip;
use super::tailscale::{NodeAddresses, Tailscale};
use crate::ports::{Result, TeamRepo};
use crate::team::link;

// ---------------------------------------------------------------------------
// Callers
// ---------------------------------------------------------------------------

type Lookup = Box<dyn Fn(SocketAddr) -> Result<WhoIs> + Send + Sync>;

/// [`CallerIdentity`] over Tailscale WhoIs. A WhoIs lookup blocks (a LocalAPI
/// round trip, or a `tailscale` process when the LocalAPI is not found), so it
/// runs on the blocking pool through [`BlockingIdentity`].
///
/// Fails closed: an address outside the tailnet is refused without asking
/// Tailscale, and a tagged node, a node without a user, or a failed lookup is
/// `not_member`. The reason goes into the error's message, which the server
/// keeps for its log and wraps before a caller sees it.
pub struct TailscaleCallers {
    inner: BlockingIdentity<Lookup>,
}

impl TailscaleCallers {
    /// Shares formationd's `tailscale` with the other adapters.
    pub fn new(tailscale: Arc<Tailscale>) -> Self {
        Self::with_resolver(tailscale)
    }

    /// Any resolver; tests use a stub instead of this Mac's Tailscale.
    pub fn with_resolver(resolver: Arc<dyn WhoIsResolver>) -> Self {
        let lookup: Lookup = Box::new(move |addr| identify_blocking(resolver.as_ref(), addr));
        Self {
            inner: BlockingIdentity::new(lookup),
        }
    }
}

#[async_trait]
impl CallerIdentity for TailscaleCallers {
    async fn identify(&self, addr: SocketAddr) -> Result<WhoIs> {
        self.inner.identify(addr).await
    }
}

fn identify_blocking(resolver: &dyn WhoIsResolver, addr: SocketAddr) -> Result<WhoIs> {
    if !is_tailscale_ip(addr.ip()) {
        return Err(not_member(format!("{addr} is not a Tailscale address")));
    }
    let resolved = resolver
        .whois(addr)
        .map_err(|error| not_member(format!("WhoIs {addr}: {error}")))?;
    let identity = resolved.identity;
    match identity.verified_user() {
        Ok(_) => identity
            .to_contract()
            .ok_or_else(|| not_member(format!("node {} has no user login", identity.node.name))),
        Err(reason) => Err(not_member(reason.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Directory
// ---------------------------------------------------------------------------

/// Where the tailnet's nodes are: each node's tailnet IPv4 by `StableID`.
/// [`Tailscale`] is the real one; tests use a stub.
pub trait NodeAddressSource: Send + Sync {
    /// Blocking; called on a helper thread, never on the async runtime.
    fn node_addresses(&self) -> std::result::Result<NodeAddresses, WhoIsError>;
}

impl NodeAddressSource for Tailscale {
    fn node_addresses(&self) -> std::result::Result<NodeAddresses, WhoIsError> {
        Tailscale::node_addresses(self)
    }
}

/// How [`TailnetDirectory`] keeps its copy of Tailscale's peer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryConfig {
    /// The least time between two reads of the peer list, however often a
    /// member's address is missing; bounds the LocalAPI or CLI calls when a
    /// teammate's Mac is not on the tailnet at all.
    pub min_refresh_interval: Duration,
    /// A peer list older than this is read again when an address is looked up.
    /// Tailnet addresses rarely change, but a member who rejoins from another
    /// Mac is reached at the new node only once the list is read again.
    pub max_age: Duration,
    /// How long one lookup waits for a read it started (or found running)
    /// before answering from what it already has.
    pub wait: Duration,
}

impl Default for DirectoryConfig {
    fn default() -> Self {
        Self {
            min_refresh_interval: Duration::from_secs(10),
            max_age: Duration::from_secs(60),
            // The LocalAPI answers in about a millisecond and the CLI in 25 to
            // 75 ms; only a hung Tailscale takes longer.
            wait: Duration::from_millis(250),
        }
    }
}

/// [`PeerDirectory`] over the team's roster and Tailscale's peer list.
///
/// The data model has no address per member, only the `StableID` of the node
/// each member joined from (`Member::tailscale_node`, checked on every peer
/// call). The address is that node's tailnet IPv4 in Tailscale's status, with
/// [`PEER_PORT`]. For the creator, the address in the team record (the
/// invitation link's host and port) is the fallback only while Tailscale's
/// list cannot be read (no read has succeeded yet, or the last one failed).
/// A list read successfully that leaves the creator's node out is believed:
/// the node may have been tagged or reassigned, and the invitation's host
/// most likely names that same node, which must not be sent a member's
/// questions.
///
/// Only current members have an address. [`TeamRepo::member`] also finds
/// removed members, so the lookup goes through the roster instead: the
/// outbox keeps retrying a removed member's entries, and their Mac may still
/// accept them until it hears of the removal.
///
/// The [`PeerDirectory`] methods are synchronous and the peer client calls
/// them from async tasks, on every call and reconnect. So the peer list is
/// cached, and read again only when a member's node is missing from it or it
/// is older than [`DirectoryConfig::max_age`], at most once per
/// [`DirectoryConfig::min_refresh_interval`]. The read itself runs on its own
/// thread; a lookup waits for it at most [`DirectoryConfig::wait`], so a hung
/// Tailscale blocks an async worker for that long at worst, and the read's
/// answer serves the next lookup.
pub struct TailnetDirectory {
    team: Arc<dyn TeamRepo + Send + Sync>,
    cache: Arc<NodeCache>,
    config: DirectoryConfig,
}

impl TailnetDirectory {
    pub fn new(team: Arc<dyn TeamRepo + Send + Sync>, tailscale: Arc<Tailscale>) -> Self {
        Self::with_source(team, tailscale, DirectoryConfig::default())
    }

    /// Any peer-list source and timing; tests use a stub.
    pub fn with_source(
        team: Arc<dyn TeamRepo + Send + Sync>,
        source: Arc<dyn NodeAddressSource>,
        config: DirectoryConfig,
    ) -> Self {
        Self {
            team,
            cache: Arc::new(NodeCache {
                source,
                state: Mutex::new(CacheState::default()),
                read: Condvar::new(),
            }),
            config,
        }
    }

    /// Starts reading the peer list without waiting for it, so the first
    /// lookups after start find it ready. Never blocks.
    pub fn warm_up(&self) {
        let mut state = self.cache.lock();
        self.cache.start_read(&mut state, &self.config);
    }

    /// The node's address, and whether the list it came from can be trusted to
    /// say a node is absent (a read succeeded, and the last read did not fail).
    fn tailnet_address(&self, node: &str) -> (Option<Ipv4Addr>, bool) {
        let mut state = self.cache.lock();
        let fresh = state
            .read_at
            .is_some_and(|at| at.elapsed() < self.config.max_age);
        if let Some(ip) = state.nodes.get(node).copied()
            && fresh
        {
            return (Some(ip), true);
        }
        let generation = state.generation;
        let reading = self.cache.start_read(&mut state, &self.config);
        if reading {
            state = self
                .cache
                .wait_for_read(state, generation, self.config.wait);
        }
        // A stale address is still the best guess while the list cannot be read.
        let listed = state.read_at.is_some() && !state.last_read_failed;
        (state.nodes.get(node).copied(), listed)
    }

    fn creator_address(&self, member: &Member) -> Option<String> {
        if member.role != MemberRole::Creator {
            return None;
        }
        let team = self.team.team().ok()??;
        (!team.creator_host.is_empty())
            .then(|| link::address(&team.creator_host, team.creator_port))
    }
}

impl PeerDirectory for TailnetDirectory {
    fn me(&self) -> Option<MemberId> {
        self.team.me().ok().flatten()
    }

    fn members(&self) -> Vec<Member> {
        self.team.members().unwrap_or_default()
    }

    fn address(&self, member: MemberId) -> Option<String> {
        // Not `TeamRepo::member`, which also finds removed members.
        let member = self
            .team
            .members()
            .ok()?
            .into_iter()
            .find(|candidate| candidate.id == member)?;
        match self.tailnet_address(&member.tailscale_node) {
            (Some(ip), _) => Some(SocketAddr::from((ip, PEER_PORT)).to_string()),
            (None, false) => self.creator_address(&member),
            (None, true) => None,
        }
    }
}

struct NodeCache {
    source: Arc<dyn NodeAddressSource>,
    state: Mutex<CacheState>,
    /// Signalled when a read finishes, successful or not.
    read: Condvar,
}

#[derive(Default)]
struct CacheState {
    nodes: HashMap<String, Ipv4Addr>,
    /// When `nodes` was last read successfully.
    read_at: Option<Instant>,
    /// Whether the last finished read failed.
    last_read_failed: bool,
    /// When a read last started, successful or not.
    started_at: Option<Instant>,
    reading: bool,
    /// Counts finished reads, so a waiter knows one finished.
    generation: u64,
}

impl NodeCache {
    fn lock(&self) -> MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a read on its own thread unless one is running or one started
    /// less than the minimum interval ago. True when a read is now running.
    fn start_read(self: &Arc<Self>, state: &mut CacheState, config: &DirectoryConfig) -> bool {
        if state.reading {
            return true;
        }
        if state
            .started_at
            .is_some_and(|at| at.elapsed() < config.min_refresh_interval)
        {
            return false;
        }
        state.reading = true;
        state.started_at = Some(Instant::now());
        let cache = self.clone();
        let spawned = std::thread::Builder::new()
            .name("formation-tailnet-peers".into())
            .spawn(move || {
                let result = cache.source.node_addresses();
                let mut state = cache.lock();
                state.last_read_failed = result.is_err();
                if let Ok(nodes) = result {
                    state.nodes = nodes;
                    state.read_at = Some(Instant::now());
                }
                // A failed read keeps the last list: the addresses are likely
                // still right, and the client treats a dead one as offline.
                state.reading = false;
                state.generation += 1;
                drop(state);
                cache.read.notify_all();
            });
        if spawned.is_err() {
            state.reading = false;
        }
        state.reading
    }

    /// Waits until a read after `generation` finishes, or `wait` passes.
    fn wait_for_read<'a>(
        &self,
        mut state: MutexGuard<'a, CacheState>,
        generation: u64,
        wait: Duration,
    ) -> MutexGuard<'a, CacheState> {
        let deadline = Instant::now() + wait;
        while state.generation == generation {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self
                .read
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use formation_proto::common::{ErrorCode, Timestamp};
    use formation_proto::team::TeamId;

    use super::*;
    use crate::collab_store::{COLLAB_MIGRATIONS, SqliteCollabStore};
    use crate::peer::identity::{PeerIdentity, Resolved, TailnetNode, TailnetUser, WhoIsSource};
    use crate::ports::TeamRecord;
    use crate::store::{MIGRATIONS, SqliteStore};

    // -- callers ------------------------------------------------------------

    /// WhoIs answers by caller IP, counting lookups.
    #[derive(Default)]
    struct StubResolver {
        answers: Mutex<HashMap<std::net::IpAddr, std::result::Result<PeerIdentity, WhoIsError>>>,
        lookups: AtomicUsize,
    }

    impl StubResolver {
        fn answer(&self, ip: &str, answer: std::result::Result<PeerIdentity, WhoIsError>) {
            self.answers
                .lock()
                .unwrap()
                .insert(ip.parse().unwrap(), answer);
        }
    }

    impl WhoIsResolver for StubResolver {
        fn whois(&self, addr: SocketAddr) -> std::result::Result<Resolved, WhoIsError> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            let answer = self.answers.lock().unwrap().get(&addr.ip()).cloned();
            answer
                .unwrap_or_else(|| Err(WhoIsError::NotFound(addr.to_string())))
                .map(|identity| Resolved {
                    identity,
                    source: WhoIsSource::LocalApi,
                })
        }
    }

    fn node_identity(stable_id: &str, login: Option<&str>, tags: &[&str]) -> PeerIdentity {
        PeerIdentity {
            node: TailnetNode {
                stable_id: stable_id.into(),
                name: format!("{stable_id}.tail0000.ts.net"),
                short_name: stable_id.into(),
                tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            },
            user: login.map(|login| TailnetUser {
                id: 7,
                login: login.into(),
                display_name: login.into(),
            }),
        }
    }

    #[tokio::test]
    async fn a_users_node_is_identified_by_login_and_stable_id() {
        let resolver = Arc::new(StubResolver::default());
        resolver.answer(
            "100.88.77.66",
            Ok(node_identity("nBob", Some("bob@example.com"), &[])),
        );
        let callers = TailscaleCallers::with_resolver(resolver.clone());
        let whois = callers
            .identify("100.88.77.66:52000".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            whois,
            WhoIs {
                login: "bob@example.com".into(),
                node: "nBob".into(),
            }
        );
    }

    #[tokio::test]
    async fn tagged_nodes_unknown_nodes_and_failures_are_not_members() {
        let resolver = Arc::new(StubResolver::default());
        resolver.answer(
            "100.99.98.97",
            Ok(node_identity(
                "nServer",
                Some("bob@example.com"),
                &["tag:server"],
            )),
        );
        resolver.answer("100.99.98.96", Ok(node_identity("nNoUser", None, &[])));
        resolver.answer(
            "100.99.98.95",
            Err(WhoIsError::Unavailable(
                "no Tailscale LocalAPI found".into(),
            )),
        );
        let callers = TailscaleCallers::with_resolver(resolver.clone());
        for (addr, expected) in [
            ("100.99.98.97:1", "tagged node"),
            ("100.99.98.96:1", "no user login"),
            ("100.99.98.95:1", "Tailscale unavailable"),
            ("100.99.98.94:1", "peer not found"),
        ] {
            let error = callers.identify(addr.parse().unwrap()).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember, "{addr}");
            assert!(
                error.message.contains(expected),
                "{addr}: {}",
                error.message
            );
        }
        assert_eq!(resolver.lookups.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn addresses_outside_the_tailnet_are_refused_without_whois() {
        let resolver = Arc::new(StubResolver::default());
        resolver.answer(
            "192.168.1.2",
            Ok(node_identity("nBob", Some("bob@example.com"), &[])),
        );
        let callers = TailscaleCallers::with_resolver(resolver.clone());
        for addr in ["192.168.1.2:52000", "127.0.0.1:52000"] {
            let error = callers.identify(addr.parse().unwrap()).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::NotMember);
        }
        assert_eq!(resolver.lookups.load(Ordering::SeqCst), 0);
    }

    // -- directory ----------------------------------------------------------

    /// A peer list that tests change, counting reads; `delay` stands in for a
    /// slow Tailscale and `fail` for one that cannot be read.
    #[derive(Default)]
    struct StubSource {
        nodes: Mutex<NodeAddresses>,
        reads: AtomicUsize,
        delay: Mutex<Duration>,
        fail: Mutex<bool>,
    }

    impl StubSource {
        fn set(&self, node: &str, ip: Option<Ipv4Addr>) {
            let mut nodes = self.nodes.lock().unwrap();
            match ip {
                Some(ip) => nodes.insert(node.to_owned(), ip),
                None => nodes.remove(node),
            };
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl NodeAddressSource for StubSource {
        fn node_addresses(&self) -> std::result::Result<NodeAddresses, WhoIsError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(*self.delay.lock().unwrap());
            if *self.fail.lock().unwrap() {
                return Err(WhoIsError::Unavailable(
                    "no Tailscale LocalAPI found".into(),
                ));
            }
            Ok(self.nodes.lock().unwrap().clone())
        }
    }

    const BOB_IP: Ipv4Addr = Ipv4Addr::new(100, 88, 77, 66);
    const CREATOR_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 1);

    fn now() -> Timestamp {
        chrono::Utc::now()
    }

    fn member(login: &str, role: MemberRole) -> Member {
        Member {
            id: MemberId::generate(),
            github_login: login.into(),
            display_name: login.into(),
            tailscale_login: format!("{login}@example.com"),
            tailscale_node: format!("n{login}"),
            role,
            joined_at: now(),
            online: false,
            last_seen_at: None,
        }
    }

    struct World {
        store: Arc<SqliteCollabStore>,
        creator: Member,
        bob: Member,
        source: Arc<StubSource>,
        directory: TailnetDirectory,
    }

    /// Bob's Mac, in a team created by `dohyeon`, whose invitation pointed at
    /// `creator_host`.
    fn world(config: DirectoryConfig, creator_host: &str) -> World {
        let sqlite = SqliteStore::in_memory(&[MIGRATIONS, COLLAB_MIGRATIONS]).unwrap();
        let store = Arc::new(SqliteCollabStore::new(Arc::new(sqlite)));
        let creator = member("dohyeon", MemberRole::Creator);
        let bob = member("bob", MemberRole::Member);
        store
            .put_team(Some(&TeamRecord {
                id: TeamId::generate(),
                creator_host: creator_host.into(),
                creator_port: 7413,
                roster_version: 2,
                open_invitation: None,
            }))
            .unwrap();
        store.put_member(&creator).unwrap();
        store.put_member(&bob).unwrap();
        store.set_me(bob.id).unwrap();
        let source = Arc::new(StubSource::default());
        let directory = TailnetDirectory::with_source(store.clone(), source.clone(), config);
        World {
            store,
            creator,
            bob,
            source,
            directory,
        }
    }

    fn quick() -> DirectoryConfig {
        DirectoryConfig {
            min_refresh_interval: Duration::from_secs(60),
            max_age: Duration::from_secs(600),
            wait: Duration::from_secs(5),
        }
    }

    #[test]
    fn the_roster_comes_from_the_team_repo() {
        let w = world(quick(), "100.64.0.1");
        assert_eq!(w.directory.me(), Some(w.bob.id));
        let mut ids: Vec<_> = w.directory.members().iter().map(|m| m.id).collect();
        ids.sort();
        let mut expected = vec![w.creator.id, w.bob.id];
        expected.sort();
        assert_eq!(ids, expected);
        assert_eq!(w.directory.address(MemberId::generate()), None);
        // No lookup of the roster reads the peer list.
        assert_eq!(w.source.reads(), 0);
    }

    #[test]
    fn a_members_address_is_their_nodes_ipv4_with_the_peer_port() {
        let w = world(quick(), "100.64.0.1");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        // Cached: the second lookup does not read the list again.
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 1);
    }

    #[test]
    fn a_missing_node_rereads_the_list_at_most_once_per_interval() {
        let config = DirectoryConfig {
            min_refresh_interval: Duration::from_millis(200),
            ..quick()
        };
        let w = world(config, "100.64.0.1");
        assert_eq!(w.directory.address(w.bob.id), None);
        assert_eq!(w.source.reads(), 1);

        // Bob's Mac comes onto the tailnet, but the last read was just now.
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        assert_eq!(w.directory.address(w.bob.id), None);
        assert_eq!(w.source.reads(), 1);

        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 2);
    }

    #[test]
    fn a_stale_list_is_read_again() {
        let config = DirectoryConfig {
            min_refresh_interval: Duration::ZERO,
            max_age: Duration::from_millis(100),
            ..quick()
        };
        let w = world(config, "100.64.0.1");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        assert!(w.directory.address(w.bob.id).is_some());

        // Bob rejoined from a Mac whose node has another address.
        w.source
            .set(&w.bob.tailscale_node, Some(Ipv4Addr::new(100, 88, 77, 99)));
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.99:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 2);
    }

    #[test]
    fn a_failed_read_keeps_the_last_list() {
        let config = DirectoryConfig {
            min_refresh_interval: Duration::ZERO,
            max_age: Duration::ZERO,
            ..quick()
        };
        let w = world(config, "100.64.0.1");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        assert!(w.directory.address(w.bob.id).is_some());
        *w.source.fail.lock().unwrap() = true;
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 2);
    }

    #[test]
    fn the_creator_falls_back_to_the_invitation_address() {
        let w = world(quick(), "dohyeon-mbp.tail0000.ts.net");
        *w.source.fail.lock().unwrap() = true;
        assert_eq!(
            w.directory.address(w.creator.id),
            Some("dohyeon-mbp.tail0000.ts.net:7413".into())
        );
        // Other members have no such fallback.
        assert_eq!(w.directory.address(w.bob.id), None);

        // The node in the list wins over the invitation's host.
        let w = world(quick(), "dohyeon-mbp.tail0000.ts.net");
        w.source.set(&w.creator.tailscale_node, Some(CREATOR_IP));
        assert_eq!(
            w.directory.address(w.creator.id),
            Some(format!("100.64.0.1:{PEER_PORT}"))
        );
        // A team record without a host gives no address.
        w.store
            .put_team(Some(&TeamRecord {
                id: TeamId::generate(),
                creator_host: String::new(),
                creator_port: 7413,
                roster_version: 2,
                open_invitation: None,
            }))
            .unwrap();
        w.source.set(&w.creator.tailscale_node, None);
        let fresh = TailnetDirectory::with_source(w.store.clone(), w.source.clone(), quick());
        assert_eq!(fresh.address(w.creator.id), None);
    }

    #[test]
    fn a_list_that_leaves_out_the_creators_node_is_believed() {
        let w = world(quick(), "dohyeon-mbp.tail0000.ts.net");
        // Read successfully; the creator's node is not in it (tagged, say).
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        assert_eq!(w.directory.address(w.creator.id), None);
        assert_eq!(w.source.reads(), 1);

        // Once a read fails, the invitation's host is the best guess again.
        let config = DirectoryConfig {
            min_refresh_interval: Duration::ZERO,
            max_age: Duration::ZERO,
            ..quick()
        };
        let w = world(config, "dohyeon-mbp.tail0000.ts.net");
        assert_eq!(w.directory.address(w.creator.id), None);
        *w.source.fail.lock().unwrap() = true;
        assert_eq!(
            w.directory.address(w.creator.id),
            Some("dohyeon-mbp.tail0000.ts.net:7413".into())
        );
    }

    #[test]
    fn removed_members_have_no_address() {
        let w = world(quick(), "dohyeon-mbp.tail0000.ts.net");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        w.source.set(&w.creator.tailscale_node, Some(CREATOR_IP));
        assert!(w.directory.address(w.bob.id).is_some());
        assert!(w.directory.address(w.creator.id).is_some());
        w.store.remove_member(w.bob.id, now()).unwrap();
        w.store.remove_member(w.creator.id, now()).unwrap();
        assert_eq!(w.directory.address(w.bob.id), None);
        assert_eq!(w.directory.address(w.creator.id), None);

        // Nor does a removed creator fall back to the invitation's host.
        let w = world(quick(), "dohyeon-mbp.tail0000.ts.net");
        *w.source.fail.lock().unwrap() = true;
        assert!(w.directory.address(w.creator.id).is_some());
        w.store.remove_member(w.creator.id, now()).unwrap();
        assert_eq!(w.directory.address(w.creator.id), None);
    }

    #[test]
    fn a_slow_read_answers_the_next_lookup_without_blocking_this_one() {
        let config = DirectoryConfig {
            wait: Duration::from_millis(20),
            ..quick()
        };
        let w = world(config, "100.64.0.1");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        *w.source.delay.lock().unwrap() = Duration::from_millis(300);
        let started = Instant::now();
        assert_eq!(w.directory.address(w.bob.id), None);
        assert!(started.elapsed() < Duration::from_millis(200));

        // The read keeps going and fills the cache; a lookup now waits on that
        // read rather than starting another.
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 1);
    }

    #[test]
    fn warm_up_reads_the_list_in_the_background() {
        let w = world(quick(), "100.64.0.1");
        w.source.set(&w.bob.tailscale_node, Some(BOB_IP));
        *w.source.delay.lock().unwrap() = Duration::from_millis(500);
        let started = Instant::now();
        w.directory.warm_up();
        assert!(started.elapsed() < Duration::from_millis(400));
        // The lookup joins the read warm_up started instead of starting one.
        assert_eq!(
            w.directory.address(w.bob.id),
            Some(format!("100.88.77.66:{PEER_PORT}"))
        );
        assert_eq!(w.source.reads(), 1);
    }
}
