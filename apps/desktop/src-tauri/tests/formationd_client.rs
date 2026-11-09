//! The app's socket client against fake-formationd over a real Unix socket.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fake_formationd::fixtures::{Fixtures, default_dir};
use fake_formationd::hub::{Hub, serve};
use formation_desktop_lib::formationd::{Connection, Sink, Status};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

enum Msg {
    Event(Value),
    Status(Status),
}

struct ChannelSink(UnboundedSender<Msg>);

impl Sink for ChannelSink {
    fn event(&self, envelope: Value) {
        let _ = self.0.send(Msg::Event(envelope));
    }
    fn status(&self, status: Status) {
        let _ = self.0.send(Msg::Status(status));
    }
}

fn fake(state: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ffd-client-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let hub = Hub::new(Fixtures::load(&default_dir()).unwrap(), state).unwrap();
    std::thread::spawn(move || serve(Arc::new(Mutex::new(hub)), listener));
    path
}

async fn next(rx: &mut UnboundedReceiver<Msg>) -> Msg {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out")
        .expect("closed")
}

async fn until_status(rx: &mut UnboundedReceiver<Msg>, connected: bool) {
    loop {
        if let Msg::Status(s) = next(rx).await
            && s.connected == connected
        {
            return;
        }
    }
}

#[tokio::test]
async fn calls_events_errors_and_reconnect() {
    let path = fake("SC-005/approval");
    let (tx, mut rx) = unbounded_channel();
    let conn = Connection::start(path.clone(), ChannelSink(tx));
    until_status(&mut rx, true).await;
    assert!(conn.status().connected);

    // Snapshot, then the state's six replayed events in order.
    let snapshot = conn.call("snapshot", json!({})).await.expect("snapshot");
    assert_eq!(snapshot["agents"].as_array().unwrap().len(), 6);
    let base = snapshot["seq"].as_u64().unwrap();
    let mut seqs = Vec::new();
    while seqs.len() < 6 {
        if let Msg::Event(e) = next(&mut rx).await {
            seqs.push(e["seq"].as_u64().unwrap());
        }
    }
    assert_eq!(seqs, (base + 1..=base + 6).collect::<Vec<_>>());

    // A command's result, then its events.
    let item = conn
        .call(
            "agent_message_send",
            json!({ "agent_id": "01930000-0000-7000-8000-000000000101", "text": "hi" }),
        )
        .await
        .expect("send");
    assert_eq!(item["text"], "hi");

    // An error keeps formationd's code and ApiError.
    let err = conn.call("team_invite", json!({})).await.unwrap_err();
    assert_eq!(err.code, "rpc");
    assert_eq!(err.rpc_code, Some(-32000));
    assert_eq!(err.data.unwrap()["code"], "internal");

    // Switching the fake's state drops this connection (it took a snapshot), so
    // the reply may not arrive; the client then reconnects.
    let _ = conn
        .call("fake_state", json!({ "state": "SC-003/empty" }))
        .await;
    until_status(&mut rx, false).await;
    until_status(&mut rx, true).await;
    let snapshot = conn
        .call("snapshot", json!({}))
        .await
        .expect("snapshot again");
    assert!(snapshot["agents"].as_array().unwrap().is_empty());
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_call_without_formationd_fails_fast() {
    let path = std::env::temp_dir().join(format!("ffd-none-{}.sock", std::process::id()));
    let (tx, _rx) = unbounded_channel();
    let conn = Connection::start(path, ChannelSink(tx));
    let err = conn.call("snapshot", json!({})).await.unwrap_err();
    assert_eq!(err.code, "disconnected");
    assert!(!conn.status().connected);
}
