mod fake_commands;
mod no_team;

use std::io;
use std::path::Path;
use std::sync::Arc;

use formation_core::agents::{Agents, ClaudeReadiness, HarnessReadiness};
use formation_core::harness::claude::{ClaudeConfig, ClaudeSupervisor};
use formation_core::harness::detect::{Detect, DetectPaths};
use formation_core::notify::{AttentionNotifier, monitor};
use formation_core::ports::{
    CollabStore, CommandHandler, Harness, Notifier, Peers, PermissionGate, Policy, RequestQueue,
    Store,
};
use formation_core::schedules::Schedules;
use formation_core::service::{
    EventBus, Router, SnapshotBuilder, SocketServer, default_socket_path,
};
use formation_core::store::{MIGRATIONS, SqliteStore};
use formation_core::testkit::FakeHarness;
use formation_core::testkit::collab::{
    FakePeers, FakePermissionGate, FakePolicy, FakeRequestQueue, MemoryCollabStore, samples,
};
use formation_proto::agent::{HarnessKind, HarnessStatus, StopMode};
use formation_proto::team::MemberRole;
use tokio::signal::unix::{SignalKind, signal};

use fake_commands::FakeCommands;
use no_team::NoTeam;

type Error = Box<dyn std::error::Error + Send + Sync>;

struct Service {
    router: Arc<Router>,
    snapshots: Arc<SnapshotBuilder>,
    bus: Arc<EventBus>,
    harness: Arc<dyn Harness>,
    store: Arc<dyn Store>,
    notifier: Arc<dyn Notifier>,
    schedules: Arc<Schedules>,
    _policy: Option<Arc<dyn Policy>>,
    _gate: Option<Arc<dyn PermissionGate>>,
    _requests: Option<Arc<dyn RequestQueue>>,
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
    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(
        directory.join("formation.db"),
        &[MIGRATIONS],
    )?);
    let bus = Arc::new(EventBus::new(256));
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
        let supervisor = Arc::new(ClaudeSupervisor::new(config, store.clone(), bus.clone()));
        (supervisor.clone(), supervisor, Arc::new(readiness))
    };
    let agents = Arc::new(Agents::new(
        directory.join("agents"),
        store.clone(),
        bus.clone(),
        readiness,
    )?);
    let no_team = Arc::new(NoTeam::new());
    let collab: Arc<dyn CollabStore> = if fakes.contains(&"collab") || fakes.contains(&"peers") {
        Arc::new(MemoryCollabStore::new())
    } else {
        no_team.clone()
    };
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
        no_team
    };
    let notifier: Arc<dyn Notifier> = Arc::new(AttentionNotifier::new(store.clone(), bus.clone()));
    let schedules = Arc::new(Schedules::new(
        store.clone(),
        harness.clone(),
        bus.clone(),
        notifier.clone(),
    ));
    let detect = Arc::new(Detect::new(detect_paths, store.clone(), bus.clone()));
    let snapshots = Arc::new(SnapshotBuilder::new(
        store.clone(),
        collab,
        harness.clone(),
        peers,
    ));
    let router = Arc::new(
        Router::new(vec![agents, commands, detect, schedules.clone()]).map_err(io::Error::other)?,
    );
    Ok(Service {
        router,
        snapshots,
        bus,
        harness,
        store,
        notifier,
        schedules,
        _policy: fakes
            .contains(&"policy")
            .then(|| Arc::new(FakePolicy::new()) as Arc<dyn Policy>),
        _gate: fakes
            .contains(&"gate")
            .then(|| Arc::new(FakePermissionGate::new()) as Arc<dyn PermissionGate>),
        _requests: fakes
            .contains(&"requests")
            .then(|| Arc::new(FakeRequestQueue::new()) as Arc<dyn RequestQueue>),
    })
}

#[tokio::main]
async fn main() -> Result<(), Error> {
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
    let notification_task = tokio::spawn(async move {
        loop {
            match monitor(events, notifier.clone()).await {
                Ok(()) => break,
                Err(error) => eprintln!("attention monitor restarting: {error}"),
            }
            events = bus.subscribe();
        }
    });
    let schedules = service.schedules.clone();
    let schedule_task = tokio::spawn(async move {
        if let Err(error) = schedules.run().await {
            eprintln!("scheduler stopped: {error}");
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
        "formationd {} socket={} database={} fakes={}",
        env!("CARGO_PKG_VERSION"),
        socket.display(),
        database.display(),
        fake_list
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
