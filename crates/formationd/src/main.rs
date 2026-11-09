mod fake_commands;
mod no_team;

use std::io;
use std::path::Path;
use std::sync::Arc;

use formation_core::agents::{Agents, ClaudeReadiness, HarnessReadiness};
use formation_core::harness::claude::{ClaudeConfig, ClaudeSupervisor};
use formation_core::ports::{CommandHandler, Harness, Store};
use formation_core::service::{
    EventBus, Router, SnapshotBuilder, SocketServer, default_socket_path,
};
use formation_core::store::{MIGRATIONS, SqliteStore};
use formation_core::testkit::FakeHarness;
use formation_proto::agent::{HarnessKind, HarnessStatus, StopMode};
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
}

fn wiring(directory: &Path, fake_list: &str) -> Result<Service, Error> {
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
    ) = if fake_list.split(',').any(|fake| fake.trim() == "harness") {
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
    let snapshots = Arc::new(SnapshotBuilder::new(
        store.clone(),
        no_team.clone(),
        harness.clone(),
        no_team,
    ));
    let router = Arc::new(Router::new(vec![agents, commands]).map_err(io::Error::other)?);
    Ok(Service {
        router,
        snapshots,
        bus,
        harness,
        store,
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
    let service = wiring(&directory, &fake_list)?;
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
    std::fs::remove_file(socket)?;
    result?;
    Ok(())
}

#[cfg(test)]
mod tests;
