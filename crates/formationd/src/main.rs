mod fake_commands;
mod identity;
mod login_path;
mod owner_emails;

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use formation_core::agents::{Agents, ClaudeReadiness, HarnessReadiness};
use formation_core::ask::pipeline::AskPipeline;
use formation_core::ask::scorer::ClaudeScorer;
use formation_core::ask::{AskCommands, OwnerAsks, ShareHook};
use formation_core::collab_store::{COLLAB_MIGRATIONS, SqliteCollabStore};
use formation_core::harness::claude::{ClaudeConfig, ClaudeSupervisor};
use formation_core::harness::codex::{CodexConfig, CodexSupervisor};
use formation_core::harness::detect::{Detect, DetectPaths};
use formation_core::harness::dispatch::HarnessDispatcher;
use formation_core::history::History;
use formation_core::notify::{AttentionNotifier, monitor, reconcile};
use formation_core::peer::api::{
    AskEndpoints, CallerIdentity, JoinEndpoint, PeerApi, PeerApiConfig, PeerApiDeps, PeerEventHub,
    RequestEndpoints,
};
use formation_core::peer::client::{PeerClient, PeerClientConfig, PeerDirectory};
use formation_core::peer::outbox::{MemoryOutboxStore, OutboxStore};
use formation_core::peer::{TailnetDirectory, Tailscale, TailscaleCallers, Via};
use formation_core::permission::{Gate, PermissionCommands};
use formation_core::policy::TeamPolicy;
use formation_core::policy::approvals::{ApprovalStore, Approvals, SystemClock};
use formation_core::policy::sharing::ShareChange;
use formation_core::ports::{
    CollabStore, CommandHandler, Harness, Notifier, Peers, PermissionGate, Policy, RequestQueue,
    Store, TeamRepo,
};
use formation_core::requests::peer::{PeerRequests, forward_updates};
use formation_core::requests::{RequestClient, Requests};
use formation_core::schedules::Schedules;
use formation_core::service::{
    EventBus, Router, SnapshotBuilder, SocketServer, default_socket_path,
};
use formation_core::store::{MIGRATIONS, SqliteStore};
use formation_core::team::{
    LocalIdentity, TeamConfig, TeamJoinEndpoint, TeamService, forward_roster, system_clock,
};
use formation_core::testkit::FakeHarness;
use formation_core::testkit::collab::{
    FakePeers, FakePermissionGate, FakePolicy, FakeRequestQueue, MemoryCollabStore, samples,
};
use formation_proto::agent::{HarnessKind, HarnessStatus, StopMode};
use formation_proto::common::AgentId;
use formation_proto::peer::{PEER_PORT, PeerEvent, RosterResponse};
use formation_proto::request::RequestStatus;
use formation_proto::team::{Member, MemberRole};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use fake_commands::FakeCommands;
use identity::TailnetIdentity;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// One collaboration store seen as each port that needs it.
type CollabStores = (
    Arc<dyn CollabStore>,
    Arc<dyn ApprovalStore>,
    Arc<dyn TeamRepo + Send + Sync>,
    Arc<dyn OutboxStore>,
);

struct Service {
    router: Arc<Router>,
    snapshots: Arc<SnapshotBuilder>,
    bus: Arc<EventBus>,
    harness: Arc<dyn Harness>,
    store: Arc<dyn Store>,
    notifier: Arc<dyn Notifier>,
    schedules: Arc<Schedules>,
    approvals: Arc<Approvals>,
    peers: Arc<dyn Peers>,
    /// The real peer client; `None` with `FORMATION_FAKE=peers`.
    peer_client: Option<Arc<PeerClient>>,
    /// What the peer server serves; `None` with `FORMATION_FAKE=peers`, whose
    /// roster is not a real team.
    peer_server: Option<PeerServer>,
    hub: PeerEventHub,
    team: Arc<TeamService<dyn CollabStore>>,
    owner_requests: Arc<Requests>,
    request_client: Arc<RequestClient>,
    ask_commands: Arc<AskCommands>,
    /// The fake harness with `FORMATION_FAKE=harness`, so tests can script agents.
    #[cfg(test)]
    fake_harness: Option<Arc<FakeHarness>>,
}

/// Builds the teammates' directory over the team store `wiring` opens.
type DirectoryFactory = Box<dyn FnOnce(Arc<dyn TeamRepo + Send + Sync>) -> Arc<dyn PeerDirectory>>;

/// How this Mac knows itself and reaches its teammates. formationd uses
/// Tailscale ([`Tailnet::tailscale`]); the loopback test injects fixed answers.
struct Tailnet {
    /// This Mac's own node, host and GitHub login, for the team service.
    local: Arc<dyn LocalIdentity>,
    /// Who is calling the peer server.
    callers: Arc<dyn CallerIdentity>,
    directory: DirectoryFactory,
    /// The port invitation links carry.
    peer_port: u16,
}

impl Tailnet {
    fn tailscale(tailscale: Arc<Tailscale>) -> Self {
        Self {
            local: Arc::new(TailnetIdentity),
            callers: Arc::new(TailscaleCallers::new(tailscale.clone())),
            directory: Box::new(move |team| {
                let directory = Arc::new(TailnetDirectory::new(team, tailscale));
                // Reads the peer list in the background, ahead of the first call.
                directory.warm_up();
                directory
            }),
            peer_port: PEER_PORT,
        }
    }
}

/// The parts of [`PeerApiDeps`], kept so each bind attempt can build them again.
struct PeerServer {
    identity: Arc<dyn CallerIdentity>,
    policy: Arc<dyn Policy>,
    team: Arc<dyn TeamRepo + Send + Sync>,
    join: Arc<dyn JoinEndpoint>,
    ask: Arc<dyn AskEndpoints>,
    requests: Arc<dyn RequestEndpoints>,
    hub: PeerEventHub,
}

impl PeerServer {
    fn deps(&self) -> PeerApiDeps {
        PeerApiDeps {
            identity: self.identity.clone(),
            policy: self.policy.clone(),
            team: self.team.clone(),
            join: Some(self.join.clone()),
            ask: Some(self.ask.clone()),
            requests: Some(self.requests.clone()),
            hub: self.hub.clone(),
        }
    }
}

fn wiring(
    directory: &Path,
    fake_list: &str,
    detect_paths: DetectPaths,
    tailnet: Tailnet,
    owner_emails: &[String],
) -> Result<Service, Error> {
    let fakes: Vec<_> = fake_list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    for fake in &fakes {
        if !["harness", "peers", "policy", "gate", "requests", "collab"].contains(fake) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported FORMATION_FAKE token: {fake}"),
            )
            .into());
        }
    }
    std::fs::create_dir_all(directory)?;
    // One database: A's repositories use it as `Store`, B's through the wrapper.
    let sqlite = Arc::new(SqliteStore::open(
        directory.join("formation.db"),
        &[MIGRATIONS, COLLAB_MIGRATIONS],
    )?);
    let store: Arc<dyn Store> = sqlite.clone();
    let bus = Arc::new(EventBus::new(256));
    // The fake roster stays in memory, so it never reaches the database.
    let (collab, approval_store, team_repo, outbox): CollabStores =
        if fakes.contains(&"collab") || fakes.contains(&"peers") {
            let memory = Arc::new(MemoryCollabStore::new());
            (
                memory.clone(),
                memory.clone(),
                memory,
                Arc::new(MemoryOutboxStore::default()),
            )
        } else {
            let sqlite = Arc::new(SqliteCollabStore::new(sqlite));
            (sqlite.clone(), sqlite.clone(), sqlite.clone(), sqlite)
        };
    let policy: Arc<dyn Policy> = if fakes.contains(&"policy") {
        Arc::new(FakePolicy::new())
    } else {
        Arc::new(TeamPolicy::new(collab.clone(), store.clone()))
    };
    let owner_requests = Arc::new(Requests::new(collab.clone(), policy.clone(), bus.clone()));
    // Before any agent takes a request: re-queues `queued` requests and fails the
    // ones a restart interrupted (backend architecture, "Recovery"). A no-op until
    // this Mac is in a team.
    owner_requests.recover()?;
    let requests: Arc<dyn RequestQueue> = if fakes.contains(&"requests") {
        Arc::new(FakeRequestQueue::new())
    } else {
        owner_requests.clone()
    };
    let notifier: Arc<dyn Notifier> = Arc::new(AttentionNotifier::new(store.clone(), bus.clone()));
    // Approval moves reach the requester through `forward_updates` too.
    let approvals = Arc::new(
        Approvals::new(
            approval_store.clone(),
            bus.clone(),
            notifier.clone(),
            Arc::new(SystemClock),
        )
        .with_request_watcher(owner_requests.clone()),
    );
    #[cfg(test)]
    let mut fake_harness = None;
    let (harness, commands, readiness): (
        Arc<dyn Harness>,
        Arc<dyn CommandHandler>,
        Arc<dyn HarnessReadiness>,
    ) = if fakes.contains(&"harness") {
        let status = HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: true,
            signed_in: Some(true),
            version: Some("fake".into()),
            path: None,
            problem: None,
        };
        let harness = Arc::new(FakeHarness::new(
            HarnessKind::ClaudeCode,
            status.clone(),
            [],
        ));
        #[cfg(test)]
        {
            fake_harness = Some(harness.clone());
        }
        let commands = Arc::new(FakeCommands {
            harness: harness.clone(),
            store: store.clone(),
            status,
        });
        (harness, commands.clone(), commands)
    } else {
        let readiness = ClaudeReadiness::default();
        let mut config = ClaudeConfig::default();
        if let Some(binary) = std::env::var_os("FORMATION_CLAUDE_BIN") {
            config.binary = binary.into();
        }
        // The permission tool and hook (`formation`) must reach this daemon's socket.
        config.env.insert(
            "FORMATION_SOCKET".into(),
            default_socket_path()?.to_string_lossy().into_owned(),
        );
        // Prefer the `formation` built next to this executable (development and
        // bundles); otherwise `formation` is looked up on PATH.
        let sibling = std::env::current_exe()?.with_file_name("formation");
        if sibling.is_file() {
            let path = sibling.to_string_lossy();
            config.mcp_config = serde_json::json!({"mcpServers": {"formation": {
                "command": path, "args": ["mcp", "--permission"]
            }}})
            .to_string();
            config.pre_tool_use_command =
                format!("'{}' hook pre-tool-use", path.replace('\'', "'\\''"));
        }
        let supervisor = Arc::new(ClaudeSupervisor::with_request_queue(
            config,
            store.clone(),
            bus.clone(),
            Some(requests),
        ));
        (supervisor.clone(), supervisor, Arc::new(readiness))
    };
    // Only Claude Code's permission tool and hook ask by run ID, so the gate looks
    // runs up in Claude Code's harness; Codex asks the gate directly.
    let gate = Arc::new(Gate::new(
        harness.clone(),
        store.clone(),
        approval_store,
        policy.clone(),
        approvals.clone(),
    ));
    let codex_gate: Arc<dyn PermissionGate> = if fakes.contains(&"gate") {
        Arc::new(FakePermissionGate::new())
    } else {
        gate.clone()
    };
    let codex = Arc::new(CodexSupervisor::new(
        CodexConfig::default(),
        store.clone(),
        bus.clone(),
        Some(codex_gate),
    ));
    let dispatcher = Arc::new(HarnessDispatcher::new(
        store.clone(),
        vec![(harness, commands), (codex.clone(), codex)],
    ));
    let (harness, commands): (Arc<dyn Harness>, Arc<dyn CommandHandler>) =
        (dispatcher.clone(), dispatcher.clone());
    let agents = Arc::new(Agents::new(
        directory.join("agents"),
        store.clone(),
        bus.clone(),
        readiness,
    )?);
    let (peers, peer_client): (Arc<dyn Peers>, _) = if fakes.contains(&"peers") {
        let me = samples::member("fake-owner", MemberRole::Creator);
        let teammate = samples::member("fake-teammate", MemberRole::Member);
        // A fake roster is only installed when peers are explicitly requested.
        collab.set_me(me.id)?;
        collab.put_member(&me)?;
        collab.put_member(&teammate)?;
        let peers = Arc::new(FakePeers::new(me.id));
        peers.set_roster(vec![me, teammate.clone()]);
        peers.add_teammate(
            teammate.id,
            vec![samples::shared_agent(teammate.id, "fake-agent", true)],
        );
        (peers, None)
    } else {
        let client = Arc::new(PeerClient::new(
            (tailnet.directory)(team_repo.clone()),
            outbox,
            PeerClientConfig::default(),
        )?);
        (client.clone(), Some(client))
    };
    let team: Arc<TeamService<dyn CollabStore>> = Arc::new(TeamService::new(
        collab.clone(),
        tailnet.local,
        peers.clone(),
        bus.clone(),
        system_clock(),
        TeamConfig {
            peer_port: tailnet.peer_port,
            ..TeamConfig::default()
        },
    ));
    let request_client = Arc::new(RequestClient::new(
        collab.clone(),
        peers.clone(),
        harness.clone(),
        store.clone(),
        bus.clone(),
        owner_requests.clone(),
    ));
    let hub = PeerEventHub::new();
    // The routing scorer runs in a folder of Formation's own with no agent instructions.
    let routing = directory.join("routing");
    std::fs::create_dir_all(&routing)?;
    let mut scorer = ClaudeScorer::new(routing);
    if let Some(binary) = std::env::var_os("FORMATION_CLAUDE_BIN") {
        scorer = scorer.claude_binary(binary);
    }
    let pipeline =
        || AskPipeline::new(harness.clone(), Arc::new(scorer.clone())).with_events(bus.clone());
    let owner_asks = Arc::new(OwnerAsks::new(
        store.clone(),
        collab.clone(),
        policy.clone(),
        hub.clone(),
        pipeline(),
        owner_emails,
    ));
    // Before the peer server accepts calls: fails answers a restart interrupted.
    let recovered = owner_asks.recover()?;
    if recovered > 0 {
        eprintln!("failed {recovered} questions left unanswered by the previous run");
    }
    let share_hook: ShareHook = {
        let hub = hub.clone();
        let requests = owner_requests.clone();
        Arc::new(move |change: &ShareChange| {
            hub.agents_changed();
            if change.requests_stopped() {
                cancel_open_requests(&requests, change.after.agent_id);
            }
        })
    };
    let ask_commands = Arc::new(
        AskCommands::new(
            collab.clone(),
            peers.clone(),
            harness.clone(),
            store.clone(),
            bus.clone(),
        )
        .with_owner_pipeline(Arc::new(pipeline()))
        .with_share_hook(share_hook),
    );
    let peer_server = peer_client.as_ref().map(|_| PeerServer {
        identity: tailnet.callers,
        policy: policy.clone(),
        team: team_repo.clone(),
        join: Arc::new(TeamJoinEndpoint::new(team.clone())),
        ask: owner_asks.clone(),
        requests: Arc::new(PeerRequests::new(owner_requests.clone())),
        hub: hub.clone(),
    });
    let schedules = Arc::new(Schedules::new(
        store.clone(),
        harness.clone(),
        bus.clone(),
        notifier.clone(),
    ));
    let permissions = Arc::new(PermissionCommands::new(gate, approvals.clone()));
    let detect = Arc::new(Detect::new(detect_paths, store.clone(), bus.clone()));
    let snapshots = Arc::new(
        SnapshotBuilder::new(store.clone(), collab, harness.clone(), peers.clone())
            .with_harnesses(dispatcher),
    );
    let router = Arc::new(
        Router::new(vec![
            agents,
            commands,
            detect,
            schedules.clone(),
            permissions,
            team.clone(),
            request_client.clone(),
            ask_commands.clone(),
            Arc::new(History::new(store.clone())),
        ])
        .map_err(io::Error::other)?,
    );
    Ok(Service {
        router,
        snapshots,
        bus,
        harness,
        store,
        notifier,
        schedules,
        approvals,
        peers,
        peer_client,
        peer_server,
        hub,
        team,
        owner_requests,
        request_client,
        ask_commands,
        #[cfg(test)]
        fake_harness,
    })
}

/// `ShareHook`: requests turned off (or the agent unshared) cancels the agent's
/// teammate requests that have not started; a running one continues (FR-009).
fn cancel_open_requests(requests: &Requests, agent_id: AgentId) {
    let owned = match requests.owned() {
        Ok(owned) => owned,
        Err(error) => {
            eprintln!("canceling requests to agent {agent_id} failed: {error}");
            return;
        }
    };
    for request in owned {
        if request.agent_id == agent_id
            && matches!(
                request.status,
                RequestStatus::Received | RequestStatus::Queued
            )
            && let Err(error) = requests.cancel(None, request.id)
        {
            eprintln!("canceling request {} failed: {error}", request.id);
        }
    }
}

/// Starts the peer client and the tasks between the peer protocol and the local
/// modules. Call once, inside the runtime.
fn start_peers(service: &Service) -> Result<Vec<JoinHandle<()>>, Error> {
    let mut tasks = vec![
        forward_updates(&service.owner_requests, service.hub.clone()),
        forward_peer_events(
            service.peers.subscribe(),
            service.ask_commands.clone(),
            service.request_client.clone(),
            service.team.clone(),
        ),
    ];
    match &service.peer_client {
        Some(client) => {
            client.start()?;
            let client = client.clone();
            tasks.push(forward_roster(
                service.team.clone(),
                service.hub.clone(),
                move || client.sync_members(),
            ));
        }
        None => tasks.push(forward_roster(
            service.team.clone(),
            service.hub.clone(),
            || {},
        )),
    }
    Ok(tasks)
}

/// Feeds teammates' question and request updates into this Mac's copies, and the
/// creator's roster pushes into the team. The peer client forwards an update only
/// from the item's owner, and a roster only from the creator's Mac.
fn forward_peer_events(
    mut events: tokio::sync::broadcast::Receiver<PeerEvent>,
    asks: Arc<AskCommands>,
    requests: Arc<RequestClient>,
    team: Arc<TeamService<dyn CollabStore>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let result = match events.recv().await {
                Ok(PeerEvent::QuestionUpdate { question }) => {
                    asks.apply_update(question.target.owner(), *question)
                }
                Ok(PeerEvent::RequestUpdate { request }) => {
                    requests.apply_update(request.owner, *request)
                }
                Ok(PeerEvent::Roster { members, version }) => {
                    apply_creator_roster(&team, members, version)
                }
                Ok(_) => Ok(()),
                // Owners re-send open items when the events connection reopens.
                Err(RecvError::Lagged(missed)) => {
                    eprintln!("peer events: missed {missed} updates");
                    Ok(())
                }
                Err(RecvError::Closed) => return,
            };
            if let Err(error) = result {
                eprintln!("applying a teammate's update failed: {error}");
            }
        }
    })
}

/// Applies a roster push from the creator's Mac. A newer roster notifies
/// `forward_roster`, which runs `sync_members` so links to new teammates open.
/// Without a team (as with `FORMATION_FAKE=peers`) there is nothing to update.
fn apply_creator_roster(
    team: &TeamService<dyn CollabStore>,
    members: Vec<Member>,
    version: u64,
) -> formation_core::ports::Result<()> {
    let creator = team.team()?.and_then(|current| {
        current
            .members
            .into_iter()
            .find(|member| member.role == MemberRole::Creator)
    });
    let Some(creator) = creator else {
        return Ok(());
    };
    team.apply_roster(creator.id, RosterResponse { members, version })
        .map(|_| ())
}

/// Binds the peer server on this Mac's Tailscale IPv4 address, port 7413. Without
/// Tailscale (or when the bind fails) it retries with backoff, logging each attempt.
async fn bind_peer_api(server: PeerServer, tailscale: Arc<Tailscale>) -> PeerApi {
    const FIRST: Duration = Duration::from_secs(2);
    const MAX: Duration = Duration::from_secs(60);
    let mut wait = FIRST;
    let mut attempt = 1u32;
    loop {
        let lookup = tailscale.clone();
        let node = tokio::task::spawn_blocking(move || lookup.self_node())
            .await
            .ok()
            .flatten();
        match node {
            None => eprintln!(
                "peer server: attempt {attempt}: Tailscale is not running on this Mac; retrying in {}s",
                wait.as_secs()
            ),
            Some(node) => {
                let address = SocketAddr::from((node.ipv4, PEER_PORT));
                match PeerApi::bind(PeerApiConfig::new(address), server.deps()).await {
                    Ok(api) => {
                        eprintln!(
                            "peer server: attempt {attempt}: listening on {}",
                            api.local_addr()
                        );
                        return api;
                    }
                    Err(error) => eprintln!(
                        "peer server: attempt {attempt}: binding {address} failed: {error}; retrying in {}s",
                        wait.as_secs()
                    ),
                }
            }
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(MAX);
        attempt += 1;
    }
}

/// Expires approvals nobody answered within 24 hours: once at start (backend
/// architecture, "Recovery"), then whenever the next one is due. Approvals raised
/// later are only seen at the next check, so it looks again at least every minute.
async fn expire_approvals(approvals: Arc<Approvals>) {
    const RECHECK: std::time::Duration = std::time::Duration::from_secs(60);
    // A due approval whose expiry keeps failing must not spin the loop.
    const FLOOR: std::time::Duration = std::time::Duration::from_secs(1);
    loop {
        let failed = approvals.expire_due().await.is_err_and(|error| {
            eprintln!("approval expiry failed: {error}");
            true
        });
        let wait = match approvals.next_expiry() {
            Ok(Some(at)) => (at - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
                .min(RECHECK),
            Ok(None) => RECHECK,
            Err(error) => {
                eprintln!("approval expiry failed: {error}");
                RECHECK
            }
        };
        let wait = if failed {
            wait.max(FLOOR * 5)
        } else {
            wait.max(FLOOR)
        };
        tokio::time::sleep(wait).await;
    }
}

fn main() -> Result<(), Error> {
    // Children and binary lookups inherit this PATH, so it is set before any thread starts.
    let (path, path_source) = login_path::resolve();
    // SAFETY: no other thread exists yet; the runtime is built after this call.
    unsafe { std::env::set_var("PATH", path) };
    tokio::runtime::Runtime::new()?.block_on(serve(path_source))
}

async fn serve(path_source: login_path::Source) -> Result<(), Error> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    let directory = std::path::PathBuf::from(home).join("Library/Application Support/Formation");
    let socket = default_socket_path()?;
    let database = directory.join("formation.db");
    let fake_list = std::env::var("FORMATION_FAKE").unwrap_or_default();
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let tailscale = Arc::new(
        tokio::task::spawn_blocking(|| Tailscale::discover(Via::Auto))
            .await
            .map_err(io::Error::other)?,
    );
    // `git` and `gh` may block for a few seconds each.
    let owner_emails = tokio::task::spawn_blocking(owner_emails::collect)
        .await
        .map_err(io::Error::other)?;
    let mut service = wiring(
        &directory,
        &fake_list,
        DetectPaths::default(),
        Tailnet::tailscale(tailscale.clone()),
        &owner_emails,
    )?;
    let peer_tasks = start_peers(&service)?;
    let peer_api_task = service
        .peer_server
        .take()
        .map(|server| tokio::spawn(bind_peer_api(server, tailscale)));
    let mut events = service.bus.subscribe();
    let bus = service.bus.clone();
    let notifier = service.notifier.clone();
    let store = service.store.clone();
    let notification_task = tokio::spawn(async move {
        loop {
            if let Err(error) = reconcile(store.as_ref(), notifier.as_ref()).await {
                eprintln!("attention reconcile failed: {error}");
            }
            match monitor(events, notifier.clone()).await {
                Ok(()) => break,
                Err(error) => eprintln!("attention monitor restarting: {error}"),
            }
            events = bus.subscribe();
        }
    });
    let schedules = service.schedules.clone();
    // The previous process's runs are gone; their pending approvals can never be
    // answered, so close them before anything new is raised.
    match service.approvals.close_interrupted().await {
        Ok(closed) if !closed.is_empty() => {
            eprintln!("closed {} approvals left by the previous run", closed.len())
        }
        Ok(_) => {}
        Err(error) => eprintln!("closing interrupted approvals failed: {error}"),
    }
    let expiry_task = tokio::spawn(expire_approvals(service.approvals.clone()));
    let schedule_task = tokio::spawn(async move {
        while let Err(error) = schedules.run().await {
            eprintln!("scheduler restarting: {error}");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    let server = SocketServer::bind(
        &socket,
        service.router.clone(),
        service.snapshots.clone(),
        service.bus.clone(),
    )
    .await?;
    eprintln!(
        "formationd {} socket={} database={} fakes={} path_source={}",
        env!("CARGO_PKG_VERSION"),
        socket.display(),
        database.display(),
        fake_list,
        path_source
    );
    let result = tokio::select! {
        result = server.run() => result,
        _ = terminate.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    schedule_task.abort();
    expiry_task.abort();
    if let Some(task) = peer_api_task {
        if task.is_finished() {
            if let Ok(api) = task.await
                && let Err(error) = api.shutdown().await
            {
                eprintln!("peer server shutdown failed: {error}");
            }
        } else {
            task.abort();
        }
    }
    for task in peer_tasks {
        task.abort();
    }
    if let Some(client) = &service.peer_client {
        client.shutdown();
    }
    for agent in service.store.agents()? {
        if service
            .harness
            .session_info(agent.id)
            .await
            .ok()
            .flatten()
            .is_some()
            && let Err(error) = service.harness.stop(agent.id, StopMode::Now).await
        {
            eprintln!("failed to stop agent {}: {error}", agent.id);
        }
    }
    notification_task.abort();
    std::fs::remove_file(socket)?;
    result?;
    Ok(())
}

#[cfg(test)]
mod loopback_tests;
#[cfg(test)]
mod tests;
