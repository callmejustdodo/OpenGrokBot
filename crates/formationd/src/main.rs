mod fake_commands;
mod identity;
mod login_path;
mod no_team;
mod owner_only;

use std::io;
use std::path::Path;
use std::sync::Arc;

use formation_core::agents::{Agents, ClaudeReadiness, HarnessReadiness};
use formation_core::collab_store::{COLLAB_MIGRATIONS, SqliteCollabStore};
use formation_core::harness::claude::{ClaudeConfig, ClaudeSupervisor};
use formation_core::harness::codex::{CodexConfig, CodexSupervisor};
use formation_core::harness::detect::{Detect, DetectPaths};
use formation_core::harness::dispatch::HarnessDispatcher;
use formation_core::notify::{AttentionNotifier, monitor, reconcile};
use formation_core::permission::{Gate, PermissionCommands};
use formation_core::policy::approvals::{ApprovalStore, Approvals, SystemClock};
use formation_core::ports::{
    CollabStore, CommandHandler, Harness, Notifier, Peers, PermissionGate, Policy, RequestQueue,
    Store,
};
use formation_core::requests::{RequestClient, Requests};
use formation_core::schedules::Schedules;
use formation_core::service::{
    EventBus, Router, SnapshotBuilder, SocketServer, default_socket_path,
};
use formation_core::store::{MIGRATIONS, SqliteStore};
use formation_core::team::{TeamConfig, TeamService, system_clock};
use formation_core::testkit::FakeHarness;
use formation_core::testkit::collab::{
    FakePeers, FakePermissionGate, FakePolicy, FakeRequestQueue, MemoryCollabStore, samples,
};
use formation_proto::agent::{HarnessKind, HarnessStatus, StopMode};
use formation_proto::team::MemberRole;
use tokio::signal::unix::{SignalKind, signal};

use fake_commands::FakeCommands;
use identity::TailnetIdentity;
use no_team::NoTeam;
use owner_only::OwnerOnly;

type Error = Box<dyn std::error::Error + Send + Sync>;

struct Service {
    router: Arc<Router>,
    snapshots: Arc<SnapshotBuilder>,
    bus: Arc<EventBus>,
    harness: Arc<dyn Harness>,
    store: Arc<dyn Store>,
    notifier: Arc<dyn Notifier>,
    schedules: Arc<Schedules>,
}

fn wiring(directory: &Path, fake_list: &str, detect_paths: DetectPaths) -> Result<Service, Error> {
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
    let (collab, approval_store): (Arc<dyn CollabStore>, Arc<dyn ApprovalStore>) =
        if fakes.contains(&"collab") || fakes.contains(&"peers") {
            let memory = Arc::new(MemoryCollabStore::new());
            (memory.clone(), memory)
        } else {
            let sqlite = Arc::new(SqliteCollabStore::new(sqlite));
            (sqlite.clone(), sqlite)
        };
    let policy: Arc<dyn Policy> = if fakes.contains(&"policy") {
        Arc::new(FakePolicy::new())
    } else {
        Arc::new(OwnerOnly)
    };
    let owner_requests = Arc::new(Requests::new(collab.clone(), policy.clone(), bus.clone()));
    let requests: Arc<dyn RequestQueue> = if fakes.contains(&"requests") {
        Arc::new(FakeRequestQueue::new())
    } else {
        owner_requests.clone()
    };
    let notifier: Arc<dyn Notifier> = Arc::new(AttentionNotifier::new(store.clone(), bus.clone()));
    let approvals = Arc::new(Approvals::new(
        approval_store.clone(),
        bus.clone(),
        notifier.clone(),
        Arc::new(SystemClock),
    ));
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
        policy,
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
    let peers: Arc<dyn Peers> = if fakes.contains(&"peers") {
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
        peers
    } else {
        Arc::new(NoTeam::new())
    };
    let team = Arc::new(TeamService::new(
        collab.clone(),
        Arc::new(TailnetIdentity),
        peers.clone(),
        bus.clone(),
        system_clock(),
        TeamConfig::default(),
    ));
    let request_client = Arc::new(RequestClient::new(
        collab.clone(),
        peers.clone(),
        harness.clone(),
        store.clone(),
        bus.clone(),
        owner_requests,
    ));
    let schedules = Arc::new(Schedules::new(
        store.clone(),
        harness.clone(),
        bus.clone(),
        notifier.clone(),
    ));
    let permissions = Arc::new(PermissionCommands::new(gate, approvals));
    let detect = Arc::new(Detect::new(detect_paths, store.clone(), bus.clone()));
    let snapshots = Arc::new(
        SnapshotBuilder::new(store.clone(), collab, harness.clone(), peers)
            .with_harnesses(dispatcher),
    );
    let router = Arc::new(
        Router::new(vec![
            agents,
            commands,
            detect,
            schedules.clone(),
            permissions,
            team,
            request_client,
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
    })
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
    let service = wiring(&directory, &fake_list, DetectPaths::default())?;
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
mod tests;
