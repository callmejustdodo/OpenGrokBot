use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use formation_proto::common::*;
use formation_proto::events::EventEnvelope;
use formation_proto::snapshot::SnapshotGet;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;

use super::{EventBus, Router, SnapshotBuilder, decode_request, rpc_error};

/// The local socket override takes precedence over the macOS application support path.
pub fn default_socket_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("FORMATION_SOCKET") {
        return Ok(path.into());
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/Formation/formationd.sock"))
}

pub struct SocketServer {
    listener: UnixListener,
    router: Arc<Router>,
    snapshots: Arc<SnapshotBuilder>,
    bus: Arc<EventBus>,
}

impl SocketServer {
    pub async fn bind(
        path: impl AsRef<Path>,
        router: Arc<Router>,
        snapshots: Arc<SnapshotBuilder>,
        bus: Arc<EventBus>,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                match UnixStream::connect(path).await {
                    Ok(_) => {
                        return Err(io::Error::new(io::ErrorKind::AddrInUse, "socket is in use"));
                    }
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                        std::fs::remove_file(path)?
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "socket path is not a socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            router,
            snapshots,
            bus,
        })
    }

    /// Dropping this future aborts all of its connection tasks.
    pub async fn run(self) -> io::Result<()> {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    let router = self.router.clone();
                    let snapshots = self.snapshots.clone();
                    let bus = self.bus.clone();
                    connections.spawn(async move { connection(stream, router, snapshots, bus).await });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }
}

type Subscription = (Seq, broadcast::Receiver<EventEnvelope>);

pub(super) async fn connection(
    stream: UnixStream,
    router: Arc<Router>,
    snapshots: Arc<SnapshotBuilder>,
    bus: Arc<EventBus>,
) -> io::Result<()> {
    let (read, mut write) = stream.into_split();
    let (out, mut incoming) = mpsc::channel::<Value>(64);
    let (activate, activated) = oneshot::channel::<Subscription>();
    let events_out = out.clone();
    let requests = async move {
        let mut read = BufReader::new(read);
        let mut line = Vec::new();
        let mut activate = Some(activate);
        while read.read_until(b'\n', &mut line).await? != 0 {
            let mut subscription = None;
            let response = match decode_request(&line) {
                Err((id, error)) => RpcResponse::Failure {
                    jsonrpc: JSONRPC_VERSION.into(),
                    id,
                    error,
                },
                Ok(request) => {
                    let result = if request.method == "snapshot" {
                        match request.parse_params::<SnapshotGet>() {
                            Err(_) => Err(rpc_error(RPC_INVALID_PARAMS, "Invalid params")),
                            Ok(_) => match snapshots.capture(&bus).await {
                                Ok((snapshot, receiver)) => {
                                    subscription = Some((snapshot.seq, receiver));
                                    Ok(serde_json::to_value(snapshot)?)
                                }
                                Err(error) => Err(RpcError::from(error)),
                            },
                        }
                    } else {
                        router.call(&request.method, request.params).await
                    };
                    match result {
                        Ok(result) => RpcResponse::Success {
                            jsonrpc: JSONRPC_VERSION.into(),
                            id: request.id,
                            result,
                        },
                        Err(error) => RpcResponse::Failure {
                            jsonrpc: JSONRPC_VERSION.into(),
                            id: Some(request.id),
                            error,
                        },
                    }
                }
            };
            line.clear();
            out.send(serde_json::to_value(response)?)
                .await
                .map_err(io::Error::other)?;
            // Queue the snapshot response before allowing the first notification.
            if let Some(subscription) = subscription
                && let Some(activate) = activate.take()
            {
                let _ = activate.send(subscription);
            }
        }
        Ok::<bool, io::Error>(activate.is_none())
    };
    let events = async move {
        let Ok((seq, mut receiver)) = activated.await else {
            // EOF before a snapshot is handled by the reader; let it drain replies.
            return std::future::pending::<io::Result<()>>().await;
        };
        loop {
            let envelope = receiver.recv().await.map_err(io::Error::other)?;
            if envelope.seq > seq {
                // Backpressure stays local to this connection. If it falls behind
                // the bus, the next recv reports Lagged and closes the connection.
                events_out.send(json!({"jsonrpc":JSONRPC_VERSION, "method":EVENT_NOTIFICATION, "params":envelope}))
                    .await.map_err(io::Error::other)?;
            }
        }
    };
    let writer = async move {
        while let Some(value) = incoming.recv().await {
            let mut line = serde_json::to_vec(&value)?;
            line.push(b'\n');
            write.write_all(&line).await?;
        }
        Ok(())
    };
    let mut requests = Box::pin(requests);
    let mut events = Box::pin(events);
    let mut writer = Box::pin(writer);
    tokio::select! {
        result = &mut requests => {
            let subscribed = result?;
            drop(requests);
            if subscribed {
                // Request EOF does not end the client's event subscription.
                tokio::select! {
                    result = &mut events => result,
                    result = &mut writer => result,
                }
            } else {
                // A client may finish sending before reading its responses.
                drop(events);
                writer.await
            }
        }
        // Lag or I/O failure cancels even a blocked socket write.
        result = &mut events => result,
        result = &mut writer => result,
    }
}
