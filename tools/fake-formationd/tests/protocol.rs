//! Drives a fake over a real Unix socket the way the app will: snapshot, then
//! events in order, commands with results, and a state switch that disconnects.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{DateTime, Utc};
use fake_formationd::fixtures::{Fixtures, default_dir};
use fake_formationd::hub::{Hub, serve};
use formation_proto::events::EventEnvelope;
use formation_proto::snapshot::Snapshot;
use serde_json::{Value, json};

const BACKEND: &str = "01930000-0000-7000-8000-000000000101";

fn start(state: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "fake-formationd-{}-{}.sock",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind");
    let hub = Hub::new(Fixtures::load(&default_dir()).expect("fixtures"), state).expect("hub");
    let shared = Arc::new(Mutex::new(hub));
    thread::spawn(move || serve(shared, listener));
    path
}

struct Client {
    out: UnixStream,
    lines: std::io::Lines<BufReader<UnixStream>>,
    next_id: u64,
}

impl Client {
    fn connect(path: &PathBuf) -> Self {
        let out = UnixStream::connect(path).expect("connect");
        out.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let lines = BufReader::new(out.try_clone().unwrap()).lines();
        Client {
            out,
            lines,
            next_id: 1,
        }
    }

    fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.out, "{line}").unwrap();
        id
    }

    fn next(&mut self) -> Option<Value> {
        let line = self.lines.next()?.ok()?;
        Some(serde_json::from_str(&line).expect("JSON line"))
    }

    /// Sends a request and returns its reply, collecting any events that arrive first.
    fn call(&mut self, method: &str, params: Value, events: &mut Vec<EventEnvelope>) -> Value {
        let id = self.send(method, params);
        loop {
            let msg = self.next().expect("reply");
            if msg["id"] == json!(id) {
                return msg;
            }
            events.push(event(&msg));
        }
    }

    fn event(&mut self) -> EventEnvelope {
        event(&self.next().expect("event"))
    }
}

fn event(msg: &Value) -> EventEnvelope {
    assert_eq!(msg["method"], "event", "not an event: {msg}");
    serde_json::from_value(msg["params"].clone()).expect("EventEnvelope")
}

#[test]
fn snapshot_then_the_state_scripts_in_order() {
    let path = start("SC-005/approval");
    let mut c = Client::connect(&path);
    let mut early = Vec::new();
    let reply = c.call("snapshot", json!({}), &mut early);
    assert!(early.is_empty());
    let snapshot: Snapshot = serde_json::from_value(reply["result"].clone()).expect("Snapshot");
    assert_eq!(snapshot.agents.len(), 6);

    // approval-raised has six steps; they follow the snapshot, numbered on.
    let events: Vec<EventEnvelope> = (0..6).map(|_| c.event()).collect();
    let seqs: Vec<u64> = events.iter().map(|e| e.seq.0).collect();
    let first = snapshot.seq.0 + 1;
    assert_eq!(seqs, (first..first + 6).collect::<Vec<_>>());
    assert_eq!(events[2].event.name(), "approval.raised");

    // Fixture times move so that M3's 10:53 is now.
    let backend = &snapshot.agents[0];
    let active: DateTime<Utc> = backend.last_activity_at.expect("last activity");
    let ago = Utc::now() - active;
    assert!(ago.num_seconds() >= 55 && ago.num_seconds() <= 70, "{ago}");
}

#[test]
fn sending_queues_then_streams_a_reply() {
    let path = start("SC-005/working");
    let mut c = Client::connect(&path);
    let mut events = Vec::new();
    let snapshot: Snapshot =
        serde_json::from_value(c.call("snapshot", json!({}), &mut events)["result"].clone())
            .unwrap();
    let base = snapshot.conversations[0].stream_seq;

    let reply = c.call(
        "agent_message_send",
        json!({ "agent_id": BACKEND, "text": "Run the tests too" }),
        &mut events,
    );
    assert_eq!(reply["result"]["text"], "Run the tests too");
    assert_eq!(reply["result"]["source"]["kind"], "owner");

    let idle = |e: &EventEnvelope| {
        matches!(&e.event, formation_proto::events::Event::AgentState(s)
            if s.state == formation_proto::agent::AgentState::Idle)
    };
    while !events.last().is_some_and(idle) {
        events.push(c.event());
    }
    let names: Vec<&str> = events.iter().map(|e| e.event.name()).collect();
    assert_eq!(names[0], "agent.queue");
    assert_eq!(names.last(), Some(&"agent.state"));
    // seq increases by one; stream_seq continues from the snapshot's.
    for pair in events.windows(2) {
        assert_eq!(pair[1].seq.0, pair[0].seq.0 + 1);
    }
    let stream_seqs: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.event {
            formation_proto::events::Event::AgentStream(s) => Some(s.stream_seq),
            _ => None,
        })
        .collect();
    assert_eq!(
        stream_seqs,
        (base + 1..base + 1 + stream_seqs.len() as u64).collect::<Vec<_>>()
    );
    let typed = events.iter().find_map(|e| match &e.event {
        formation_proto::events::Event::AgentStream(s) => match &s.delta {
            formation_proto::harness::StreamDelta::ItemStarted { item } => {
                Some(format!("{item:?}"))
            }
            _ => None,
        },
        _ => None,
    });
    assert!(typed.unwrap().contains("Run the tests too"));
}

#[test]
fn errors_and_built_in_agent_control() {
    let path = start("SC-005/working");
    let mut c = Client::connect(&path);
    let mut events = Vec::new();
    let missing = c.call("no_such_method", json!({}), &mut events);
    assert_eq!(missing["error"]["code"], -32601);
    let unsimulated = c.call("team_invite", json!({}), &mut events);
    assert_eq!(unsimulated["error"]["code"], -32000);
    assert_eq!(unsimulated["error"]["data"]["code"], "internal");

    let stopped = c.call(
        "agent_stop",
        json!({ "agent_id": BACKEND, "mode": "now" }),
        &mut events,
    );
    assert_eq!(stopped["result"]["state"], "stopped");
    assert_eq!(stopped["result"]["name"], "Backend");
}

#[test]
fn switching_state_disconnects_clients_who_then_see_the_new_world() {
    let path = start("SC-005/working");
    let mut app = Client::connect(&path);
    let mut events = Vec::new();
    app.call("snapshot", json!({}), &mut events);

    let mut ctl = Client::connect(&path);
    let reply = ctl.call(
        "fake_state",
        json!({ "state": "SC-003/empty" }),
        &mut events,
    );
    assert_eq!(reply["result"]["state"], "SC-003/empty");
    assert!(app.next().is_none(), "the app's connection should close");

    let mut app = Client::connect(&path);
    let snapshot: Snapshot =
        serde_json::from_value(app.call("snapshot", json!({}), &mut events)["result"].clone())
            .unwrap();
    assert!(snapshot.agents.is_empty());

    let bad = ctl.call("fake_state", json!({ "state": "SC-999/none" }), &mut events);
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no state")
    );
}

#[test]
fn every_named_state_loads() {
    let fixtures = Fixtures::load(&default_dir()).unwrap();
    let names: Vec<String> = fixtures.states.keys().cloned().collect();
    let mut hub = Hub::new(fixtures, "SC-005/working").unwrap();
    for name in names {
        hub.reset(&name).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}
