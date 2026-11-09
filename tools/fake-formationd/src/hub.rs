//! The running fake: the current world, the events since it was loaded, and the
//! connected app windows. It speaks formationd's protocol on the local socket:
//! newline-delimited JSON-RPC 2.0, a `snapshot` reply, then `event`
//! notifications with increasing `seq`.
//!
//! The fake keeps no reducer. A snapshot is the loaded world plus a replay of
//! every event since, so a client that reconnects ends up in the same place.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use chrono::{SecondsFormat, TimeDelta, Utc};
use formation_proto::common::{
    EVENT_NOTIFICATION, JSONRPC_VERSION, RPC_APPLICATION_ERROR, RPC_INVALID_PARAMS,
    RPC_METHOD_NOT_FOUND, RPC_PARSE_ERROR,
};
use serde_json::{Value, json};

use crate::fixtures::{Fixtures, Placeholders, shift_times, shift_to};

type Writer = Arc<Mutex<UnixStream>>;

pub struct Hub {
    fixtures: Fixtures,
    shift: TimeDelta,
    state: String,
    /// Bumped by every reset; a script still playing from before stops.
    generation: u64,
    world: Value,
    base_seq: u64,
    seq: u64,
    log: Vec<Value>,
    /// The last `stream_seq` per agent, so every `agent.stream` event follows on.
    stream_seq: HashMap<String, u64>,
    /// Connections that took a snapshot, by connection number.
    clients: HashMap<u64, Writer>,
}

pub type Shared = Arc<Mutex<Hub>>;

impl Hub {
    pub fn new(fixtures: Fixtures, state: &str) -> Result<Self, String> {
        let mut hub = Hub {
            fixtures,
            shift: shift_to(Utc::now()),
            state: String::new(),
            generation: 0,
            world: Value::Null,
            base_seq: 0,
            seq: 0,
            log: Vec::new(),
            stream_seq: HashMap::new(),
            clients: HashMap::new(),
        };
        hub.reset(state)?;
        Ok(hub)
    }

    /// Loads a named state: its world, then its scripts at once. Connected
    /// clients are disconnected so they reconnect and take the new snapshot.
    pub fn reset(&mut self, state: &str) -> Result<(), String> {
        let st = self
            .fixtures
            .states
            .get(state)
            .ok_or_else(|| format!("no state {state}; run `fake-formationd list`"))?;
        let mut world = self.fixtures.worlds[&st.world].clone();
        let steps: Vec<Value> = st
            .scripts
            .iter()
            .flat_map(|s| {
                self.fixtures.scripts[s]
                    .steps
                    .iter()
                    .map(|step| step.event.clone())
            })
            .collect();

        shift_times(&mut world, self.shift);
        self.stream_seq = world["conversations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some((
                    c["agent_id"].as_str()?.to_owned(),
                    c["stream_seq"].as_u64()?,
                ))
            })
            .collect();
        self.world = world;
        self.base_seq = self.seq;
        self.log.clear();
        self.state = state.to_owned();
        self.generation += 1;
        for (_, out) in self.clients.drain() {
            if let Ok(out) = out.lock() {
                let _ = out.shutdown(Shutdown::Both);
            }
        }
        let ph = self.placeholders(json!({}));
        for event in steps {
            self.emit(ph.fill(&event));
        }
        Ok(())
    }

    fn placeholders(&self, params: Value) -> Placeholders {
        let now = (Utc::now() - self.shift).to_rfc3339_opts(SecondsFormat::Secs, true);
        Placeholders::at(params, now)
    }

    /// Numbers an event, records it, and sends it to every client.
    fn emit(&mut self, mut event: Value) {
        shift_times(&mut event, self.shift);
        if event["type"] == "agent.stream" {
            let agent = event["payload"]["agent_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let n = self.stream_seq.entry(agent).or_insert(0);
            *n += 1;
            event["payload"]["stream_seq"] = json!(*n);
        }
        self.seq += 1;
        let envelope = json!({
            "seq": self.seq,
            "at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            "event": event,
        });
        let line = notification(&envelope);
        self.log.push(envelope);
        self.clients.retain(|_, out| send(out, &line));
    }

    fn snapshot_lines(&self, id: &Value) -> Vec<String> {
        let mut world = self.world.clone();
        world["seq"] = json!(self.base_seq);
        let mut lines = vec![success(id, world)];
        lines.extend(self.log.iter().map(notification));
        lines
    }

    fn list(&self) -> Value {
        let states: Vec<Value> = self
            .fixtures
            .states
            .iter()
            .map(|(name, s)| json!({ "name": name, "description": s.description, "ui": s.ui }))
            .collect();
        let scripts: Vec<Value> = self
            .fixtures
            .scripts
            .iter()
            .map(|(name, s)| json!({ "name": name, "description": s.description }))
            .collect();
        json!({ "current": self.state, "states": states, "scripts": scripts })
    }
}

/// Plays steps in a background thread, each after its delay, unless the world
/// is reset meanwhile.
fn play(shared: &Shared, steps: Vec<(u64, Value)>) {
    let shared = Arc::clone(shared);
    let generation = shared.lock().map(|h| h.generation).unwrap_or(0);
    thread::spawn(move || {
        for (after_ms, event) in steps {
            thread::sleep(Duration::from_millis(after_ms));
            let Ok(mut hub) = shared.lock() else { return };
            if hub.generation != generation {
                return;
            }
            hub.emit(event);
        }
    });
}

/// Accepts connections until the process ends.
pub fn serve(shared: Shared, listener: UnixListener) {
    for (conn, stream) in (1u64..).zip(listener.incoming()) {
        let Ok(stream) = stream else { continue };
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            if let Err(e) = connection(&shared, conn, stream) {
                eprintln!("fake-formationd: connection {conn}: {e}");
            }
        });
    }
}

fn connection(shared: &Shared, conn: u64, stream: UnixStream) -> std::io::Result<()> {
    let out: Writer = Arc::new(Mutex::new(stream.try_clone()?));
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            send(
                &out,
                &failure(&Value::Null, RPC_PARSE_ERROR, "parse error", None),
            );
            continue;
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request["method"].as_str().unwrap_or_default().to_owned();
        let params = match request.get("params") {
            None | Some(Value::Null) => json!({}),
            Some(p) => p.clone(),
        };
        call(shared, conn, &out, &id, &method, params);
    }
    if let Ok(mut hub) = shared.lock() {
        hub.clients.remove(&conn);
    }
    Ok(())
}

fn call(shared: &Shared, conn: u64, out: &Writer, id: &Value, method: &str, params: Value) {
    let Ok(mut hub) = shared.lock() else { return };
    match method {
        "snapshot" => {
            // Reply and replay under the lock, so no event slips in between.
            for line in hub.snapshot_lines(id) {
                send(out, &line);
            }
            hub.clients.insert(conn, Arc::clone(out));
        }
        "fake_list" => {
            send(out, &success(id, hub.list()));
        }
        "fake_state" => {
            let Some(state) = params["state"].as_str() else {
                send(
                    out,
                    &failure(id, RPC_INVALID_PARAMS, "params.state is required", None),
                );
                return;
            };
            match hub.reset(state) {
                Ok(()) => send(out, &success(id, json!({ "state": state }))),
                Err(e) => send(out, &failure(id, RPC_INVALID_PARAMS, &e, None)),
            };
        }
        "fake_play" => {
            let name = params["script"].as_str().unwrap_or_default();
            let Some(script) = hub.fixtures.scripts.get(name) else {
                let msg = format!("no script {name}; run `fake-formationd list`");
                send(out, &failure(id, RPC_INVALID_PARAMS, &msg, None));
                return;
            };
            let ph = hub.placeholders(json!({}));
            let steps: Vec<(u64, Value)> = script
                .steps
                .iter()
                .map(|s| (s.after_ms, ph.fill(&s.event)))
                .collect();
            send(
                out,
                &success(id, json!({ "script": name, "steps": steps.len() })),
            );
            drop(hub);
            play(shared, steps);
        }
        "agent_start" | "agent_stop" | "agent_turn_interrupt" => {
            let line = agent_control(&mut hub, id, method, &params);
            send(out, &line);
        }
        _ => react(shared, hub, out, id, method, params),
    }
}

/// Start, stop, and interrupt act on whichever agent is named, so they are
/// built in instead of fixed reactions: the reply is the agent from the world
/// with its new state, followed by an `agent.state` event.
fn agent_control(hub: &mut Hub, id: &Value, method: &str, params: &Value) -> String {
    let agent_id = params["agent_id"].clone();
    let Some(mut agent) = hub.world["agents"]
        .as_array()
        .and_then(|agents| agents.iter().find(|a| a["id"] == agent_id))
        .cloned()
    else {
        return failure(
            id,
            RPC_APPLICATION_ERROR,
            "no such agent",
            Some("not_found"),
        );
    };
    let state = match method {
        "agent_stop" => "stopped",
        _ => "idle",
    };
    agent["state"] = json!(state);
    let result = if method == "agent_turn_interrupt" {
        Value::Null
    } else {
        agent
    };
    hub.emit(json!({
        "type": "agent.state",
        "payload": { "agent_id": agent_id, "state": state, "reason": null, "exit_code": null },
    }));
    success(id, result)
}

/// Answers a contract command from the reactions, or from the world for the
/// read-only lists, and plays the reaction's script.
fn react(
    shared: &Shared,
    hub: std::sync::MutexGuard<'_, Hub>,
    out: &Writer,
    id: &Value,
    method: &str,
    params: Value,
) {
    if let Some(reaction) = hub.fixtures.reaction(method, &params) {
        let ph = hub.placeholders(params.clone());
        let filled = ph.fill(&reaction.result);
        let ph = ph.with_result(filled.clone());
        let mut result = filled;
        shift_times(&mut result, hub.shift);
        let steps: Vec<(u64, Value)> = reaction
            .play
            .iter()
            .flat_map(|name| hub.fixtures.scripts[name].steps.iter())
            .map(|s| (s.after_ms, ph.fill(&s.event)))
            .collect();
        let delay = reaction.delay_ms;
        drop(hub);
        let (shared, out, id) = (Arc::clone(shared), Arc::clone(out), id.clone());
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(delay));
            send(&out, &success(&id, result));
            play(&shared, steps);
        });
        return;
    }
    let from_world = |key: &str| hub.world.get(key).cloned().unwrap_or(json!([]));
    let line = match method {
        "agents_list" => success(id, from_world("agents")),
        "teammate_agents" => success(id, from_world("teammate_agents")),
        "rules_list" => success(id, from_world("rules")),
        "harness_check" => success(id, from_world("harnesses")),
        "schedule_list" => success(id, from_world("schedules")),
        m if formation_proto::methods().any(|known| known == m) => {
            let msg = format!("fake-formationd does not simulate {m}");
            failure(id, RPC_APPLICATION_ERROR, &msg, Some("internal"))
        }
        m => failure(id, RPC_METHOD_NOT_FOUND, &format!("no method {m}"), None),
    };
    send(out, &line);
}

fn send(out: &Writer, line: &str) -> bool {
    let Ok(mut stream) = out.lock() else {
        return false;
    };
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .is_ok()
}

fn success(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": JSONRPC_VERSION, "id": id, "result": result }).to_string()
}

fn failure(id: &Value, code: i32, message: &str, api_code: Option<&str>) -> String {
    let data = api_code.map(|c| json!({ "code": c, "message": message }));
    json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": id,
        "error": { "code": code, "message": message, "data": data },
    })
    .to_string()
}

fn notification(envelope: &Value) -> String {
    json!({ "jsonrpc": JSONRPC_VERSION, "method": EVENT_NOTIFICATION, "params": envelope })
        .to_string()
}
