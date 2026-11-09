use super::*;
use async_trait::async_trait;
use formation_proto::{
    policy::Caller,
    snapshot::{Settings, SettingsUpdateEvent},
};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn event() -> Event {
    Event::SettingsUpdate(SettingsUpdateEvent {
        settings: Settings::default(),
    })
}

#[test]
fn service_bus_numbers_and_fans_out_concurrent_publications() {
    let bus = Arc::new(EventBus::new(256));
    let mut a = bus.subscribe();
    let mut b = bus.subscribe();
    let before = std::time::SystemTime::now();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let bus = bus.clone();
            scope.spawn(move || {
                for _ in 0..32 {
                    bus.publish(event());
                }
            });
        }
    });
    for seq in 1..=128 {
        let x = a.try_recv().unwrap();
        assert_eq!(x.seq, Seq(seq));
        assert_eq!(x, b.try_recv().unwrap());
        assert!(std::time::SystemTime::from(x.at) >= before);
    }
    assert_eq!(bus.last_seq(), Seq(128));
}

struct Handler;
#[async_trait]
impl CommandHandler for Handler {
    fn methods(&self) -> Vec<&'static str> {
        vec!["fail", "owner"]
    }
    async fn handle(&self, call: CommandCall) -> crate::ports::Result<Value> {
        assert_eq!(call.caller, Caller::Owner);
        if call.method == "fail" {
            Err(ApiError::new(ErrorCode::Denied, "denied"))
        } else {
            Ok(json!("owner"))
        }
    }
}

#[tokio::test]
async fn service_router_rejects_duplicates_and_preserves_errors() {
    assert!(Router::new(vec![Arc::new(Handler), Arc::new(Handler)]).is_err());
    struct SnapshotHandler;
    #[async_trait]
    impl CommandHandler for SnapshotHandler {
        fn methods(&self) -> Vec<&'static str> {
            vec!["snapshot"]
        }
        async fn handle(&self, _: CommandCall) -> crate::ports::Result<Value> {
            unreachable!()
        }
    }
    assert!(Router::new(vec![Arc::new(SnapshotHandler)]).is_err());
    let router = Router::new(vec![Arc::new(Handler)]).unwrap();
    assert_eq!(
        router.call("missing", Value::Null).await.unwrap_err().code,
        -32601
    );
    let error = serde_json::to_value(router.call("fail", Value::Null).await.unwrap_err()).unwrap();
    assert_eq!(error["code"], -32000);
    assert_eq!(error["data"]["code"], "denied");
    assert_eq!(router.call("owner", Value::Null).await.unwrap(), "owner");
}

#[test]
fn service_request_decoding_distinguishes_json_and_requests() {
    assert_eq!(decode_request(b"{").unwrap_err().1.code, -32700);
    for line in [
        "[]",
        "null",
        r#"{"jsonrpc":"1.0","id":1,"method":"owner"}"#,
        r#"{"jsonrpc":"2.0","method":"owner"}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"owner","params":5}"#,
    ] {
        assert_eq!(decode_request(line.as_bytes()).unwrap_err().1.code, -32600);
    }
}

include!("test_stubs.rs");

fn agent_id() -> AgentId {
    AgentId(uuid::Uuid::from_u128(1))
}
fn member_id() -> MemberId {
    MemberId(uuid::Uuid::from_u128(2))
}
fn agent() -> Agent {
    serde_json::from_value(json!({
        "id":agent_id(), "slug":"api", "name":"API", "role":"developer",
        "harness":"claude_code", "folder_path":"/tmp/api", "repositories":[],
        "avatar":{"shape":"round", "color":"blue"}, "state":"idle",
        "files_revision":"r1", "created_at":"2026-11-09T09:00:00Z",
        "last_activity_at":null, "detected":null
    }))
    .unwrap()
}
fn builder(bus: Arc<EventBus>, fail: bool) -> SnapshotBuilder {
    let stub = Arc::new(Stub { bus, fail });
    SnapshotBuilder::new(stub.clone(), stub.clone(), stub.clone(), stub)
}

#[tokio::test]
async fn service_snapshot_subscribes_before_reading_state_and_uses_ports() {
    let bus = Arc::new(EventBus::new(16));
    bus.publish(event());
    let (snapshot, mut receiver) = builder(bus.clone(), false).capture(&bus).await.unwrap();
    assert_eq!(snapshot.seq, Seq(1));
    assert_eq!(receiver.try_recv().unwrap().seq, Seq(2));
    assert_eq!(snapshot.agents, vec![agent()]);
    assert_eq!(snapshot.team.as_ref().unwrap().me, member_id());
    assert_eq!(snapshot.team.unwrap().invitation, None);
    assert_eq!(snapshot.shares, vec![ShareSettings::private(agent_id())]);
    assert_eq!(snapshot.conversations.len(), 1);
    assert_eq!(snapshot.conversations[0].agent_id, agent_id());
    assert!(snapshot.conversations[0].has_more);
    assert_eq!(snapshot.conversations[0].stream_seq, 7);
    assert!(snapshot.harnesses[0].installed);
    assert_eq!(
        snapshot.settings.language,
        Some(formation_proto::snapshot::Language::Ko)
    );
    assert!(builder(bus.clone(), true).capture(&bus).await.is_err());
}

struct TestServer {
    path: std::path::PathBuf,
    directory: std::path::PathBuf,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    bus: Arc<EventBus>,
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
async fn server() -> TestServer {
    let directory = std::path::PathBuf::from("/private/tmp")
        .join(format!("formation-{}", uuid::Uuid::new_v4()));
    let path = directory.join("private/service.sock");
    let bus = Arc::new(EventBus::new(8));
    let server = SocketServer::bind(
        &path,
        Arc::new(Router::new(vec![Arc::new(Handler)]).unwrap()),
        Arc::new(builder(bus.clone(), false)),
        bus.clone(),
    )
    .await
    .unwrap();
    let task = tokio::spawn(server.run());
    TestServer {
        path,
        directory,
        task,
        bus,
    }
}
async fn client(
    path: &std::path::Path,
) -> (
    tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let (read, write) = tokio::net::UnixStream::connect(path)
        .await
        .unwrap()
        .into_split();
    (tokio::io::BufReader::new(read).lines(), write)
}
async fn receive(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> Value {
    let line = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn service_socket_snapshot_events_errors_and_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let server = server().await;
    assert_eq!(
        std::fs::metadata(&server.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(server.path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    server.bus.publish(event());
    let (mut lines, mut write) = client(&server.path).await;
    write.write_all(b"{\n[]\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"missing\"}\n{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"fail\"}\n").await.unwrap();
    for code in [-32700, -32600, -32601, -32000] {
        assert_eq!(receive(&mut lines).await["error"]["code"], code);
    }
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"snapshot\",\"params\":[]}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["error"]["code"], -32602);
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    let response = receive(&mut lines).await;
    assert_eq!(response["id"], 5);
    assert_eq!(response["result"]["seq"], 1);
    // The stub publishes seq 2 while state is read: it must arrive after the snapshot.
    for seq in 2..=5 {
        if seq > 2 {
            server.bus.publish(event());
        }
        let event = receive(&mut lines).await;
        assert_eq!(event["jsonrpc"], "2.0");
        assert_eq!(event["method"], "event");
        assert_eq!(event["params"]["seq"], seq);
        assert!(event.get("id").is_none());
    }
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"owner\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"], "owner");
}

#[tokio::test]
async fn service_socket_lag_disconnects_and_reconnect_snapshot_covers_gap() {
    let server = server().await;
    let (mut lines, mut write) = client(&server.path).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"]["seq"], 0);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 1);
    // No await: overflow the receiver deterministically before its next poll.
    for _ in 0..32 {
        server.bus.publish(event());
    }
    let end = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .unwrap();
    assert!(matches!(end, Ok(None)) || end.is_err());
    let (mut lines, mut write) = client(&server.path).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"]["seq"], 33);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 34);
}

#[tokio::test]
async fn service_socket_replaces_stale_socket_but_preserves_regular_file_and_live_socket() {
    let server = server().await;
    let router = Arc::new(Router::new(vec![]).unwrap());
    let snapshots = Arc::new(builder(server.bus.clone(), false));
    assert!(
        SocketServer::bind(
            &server.path,
            router.clone(),
            snapshots.clone(),
            server.bus.clone()
        )
        .await
        .is_err()
    );
    let stale = server.directory.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    let bound = SocketServer::bind(
        &stale,
        router.clone(),
        snapshots.clone(),
        server.bus.clone(),
    )
    .await
    .unwrap();
    drop(bound);
    let file = server.directory.join("file");
    std::fs::write(&file, "keep").unwrap();
    assert!(
        SocketServer::bind(&file, router, snapshots, server.bus.clone())
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "keep");
}

fn teammate_id() -> MemberId {
    MemberId(uuid::Uuid::from_u128(3))
}
fn member(id: MemberId) -> Member {
    serde_json::from_value(json!({"id":id, "github_login":"mate", "display_name":"Mate",
        "tailscale_login":"mate", "tailscale_node":"node", "role":"member", "joined_at":"2026-11-09T09:00:00Z", "online":true, "last_seen_at":null})).unwrap()
}

#[test]
fn service_default_socket_path_uses_home_or_override() {
    // Child processes isolate environment changes from parallel tests.
    if let Ok(expected) = std::env::var("FORMATION_PATH_TEST") {
        if expected == "missing-home" {
            assert_eq!(
                default_socket_path().unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        } else {
            assert_eq!(
                default_socket_path().unwrap(),
                std::path::PathBuf::from(expected)
            );
        }
        return;
    }
    for (home, socket, expected) in [
        (
            Some("/tmp/owner"),
            None,
            "/tmp/owner/Library/Application Support/Formation/formationd.sock",
        ),
        (None, Some("/tmp/custom.sock"), "/tmp/custom.sock"),
        (None, None, "missing-home"),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "service::tests::service_default_socket_path_uses_home_or_override",
            ])
            .env("FORMATION_PATH_TEST", expected)
            .env_remove("HOME")
            .env_remove("FORMATION_SOCKET");
        if let Some(home) = home {
            child.env("HOME", home);
        }
        if let Some(socket) = socket {
            child.env("FORMATION_SOCKET", socket);
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

async fn connected_pair(
    bus: Arc<EventBus>,
) -> (
    tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let (server, client) = tokio::net::UnixStream::pair().unwrap();
    let snapshots = Arc::new(builder(bus.clone(), false));
    let task = tokio::spawn(super::socket::connection(
        server,
        Arc::new(Router::new(vec![Arc::new(Handler)]).unwrap()),
        snapshots,
        bus,
    ));
    let (read, write) = client.into_split();
    (tokio::io::BufReader::new(read).lines(), write, task)
}

#[tokio::test]
async fn service_socket_pair_delivers_response_after_write_half_closes() {
    let (mut lines, mut write, task) = connected_pair(Arc::new(EventBus::new(8))).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"owner\"}\n")
        .await
        .unwrap();
    write.shutdown().await.unwrap();
    assert_eq!(receive(&mut lines).await["result"], "owner");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn service_socket_pair_orders_snapshot_events_and_recovers_after_lag() {
    let bus = Arc::new(EventBus::new(8));
    bus.publish(event());
    let (mut lines, mut write, task) = connected_pair(bus.clone()).await;
    write
        .write_all(b"{\n[]\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["error"]["code"], -32700);
    assert_eq!(receive(&mut lines).await["error"]["code"], -32600);
    assert_eq!(receive(&mut lines).await["result"]["seq"], 1);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 2);
    for seq in 3..=5 {
        bus.publish(event());
        let notification = receive(&mut lines).await;
        assert_eq!(notification["method"], "event");
        assert_eq!(notification["params"]["seq"], seq);
        assert!(notification.get("id").is_none());
    }
    for _ in 0..32 {
        bus.publish(event());
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(lines.next_line().await.unwrap().is_none());
    let (mut lines, mut write, task) = connected_pair(bus.clone()).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"]["seq"], 37);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 38);
    task.abort();
}

#[tokio::test]
async fn service_socket_pair_keeps_events_after_request_half_closes() {
    let bus = Arc::new(EventBus::new(8));
    let (mut lines, mut write, task) = connected_pair(bus.clone()).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"]["seq"], 0);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 1);
    write.shutdown().await.unwrap();
    tokio::task::yield_now().await;
    bus.publish(event());
    assert_eq!(receive(&mut lines).await["params"]["seq"], 2);
    task.abort();
}

#[tokio::test]
async fn service_socket_pair_invalid_utf8_gets_parse_error_and_next_request_works() {
    let (mut lines, mut write, task) = connected_pair(Arc::new(EventBus::new(8))).await;
    write
        .write_all(b"\xff\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"owner\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["error"]["code"], -32700);
    assert_eq!(receive(&mut lines).await["result"], "owner");
    task.abort();
}

#[tokio::test]
async fn service_socket_pair_drains_burst_without_artificial_lag() {
    let bus = Arc::new(EventBus::new(256));
    let (mut lines, mut write, task) = connected_pair(bus.clone()).await;
    write
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"snapshot\"}\n")
        .await
        .unwrap();
    assert_eq!(receive(&mut lines).await["result"]["seq"], 0);
    assert_eq!(receive(&mut lines).await["params"]["seq"], 1);
    for _ in 0..128 {
        bus.publish(event());
    }
    for seq in 2..=129 {
        assert_eq!(receive(&mut lines).await["params"]["seq"], seq);
    }
    task.abort();
}
