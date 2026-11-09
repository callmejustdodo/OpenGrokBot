//! Replays the recorded `codex app-server` traffic in
//! `src/harness/codex/testdata/` by answering the client's requests.
//!
//! The process starts with `thread_start.jsonl` as its script. Each `turn/start`
//! loads the next fixture named in `FAKE_CODEX_TURNS` (comma separated, without
//! `.jsonl`). Server lines are sent in order, `FAKE_CODEX_LINE_DELAY_MS` apart; a
//! client line in the script waits for the matching client message. Responses keep
//! the client's request IDs, server requests are numbered from 0, and the recorded
//! thread ID and folder are replaced by the live ones. A `turn/interrupt` the script
//! does not expect ends the turn as `interrupted`. `FAKE_CODEX_LOG` appends every
//! message received and sent, in order, as `{"dir","msg"}` lines.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    env,
    error::Error,
    fs,
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const RECORDED_THREAD: &str = "01a1bed0-c036-7df2-8669-e209dbd69481";
const RECORDED_CWD: &str = "/tmp/agent";

struct Fake {
    out: io::Stdout,
    log: Option<fs::File>,
    thread_id: String,
    cwd: String,
    /// Fixture request ID and the client's ID for the request it matched.
    ids: Vec<(Value, Value)>,
    next_server_id: u64,
    turn_id: Option<String>,
}

impl Fake {
    fn record(&mut self, dir: &str, msg: &Value) -> io::Result<()> {
        if let Some(log) = &mut self.log {
            // One write per line, so a reader never sees half a record.
            log.write_all(format!("{}\n", json!({"dir": dir, "msg": msg})).as_bytes())?;
        }
        Ok(())
    }
    fn send(&mut self, msg: Value) -> io::Result<()> {
        let text = msg
            .to_string()
            .replace(RECORDED_THREAD, &self.thread_id)
            .replace(RECORDED_CWD, &self.cwd);
        let msg: Value = serde_json::from_str(&text)?;
        self.record("server", &msg)?;
        let mut out = self.out.lock();
        writeln!(out, "{msg}")?;
        out.flush()
    }
    /// Sends a recorded server line with live IDs.
    fn replay(&mut self, mut msg: Value) -> io::Result<()> {
        if msg.get("method").is_some() {
            if msg.get("id").is_some() {
                msg["id"] = json!(self.next_server_id);
                self.next_server_id += 1;
            }
        } else if let Some(id) = msg.get("id").cloned()
            && let Some((_, live)) = self.ids.iter().find(|(recorded, _)| *recorded == id)
        {
            msg["id"] = live.clone();
        }
        if let Some(id) = msg["params"]["turn"]["id"]
            .as_str()
            .or(msg["result"]["turn"]["id"].as_str())
        {
            self.turn_id = Some(id.to_owned());
        }
        self.send(msg)
    }
}

fn load(name: &str) -> Result<VecDeque<(String, Value)>, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/harness/codex/testdata")
        .join(format!("{name}.jsonl"));
    let mut lines = VecDeque::new();
    for line in fs::read_to_string(path)?.lines() {
        let record: Value = serde_json::from_str(line)?;
        lines.push_back((
            record["dir"].as_str().unwrap_or_default().to_owned(),
            record["msg"].clone(),
        ));
    }
    Ok(lines)
}

/// Whether the client's message is the one the script waits for.
fn matches(expected: &Value, got: &Value) -> bool {
    match (expected.get("method"), got.get("method")) {
        (Some(a), Some(b)) => a == b || (a == "thread/start" && b == "thread/resume"),
        (None, None) => true,
        _ => false,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let argv: Vec<_> = env::args().skip(1).collect();
    if argv == ["--version"] {
        println!("codex-cli 0.0.0 (fake-codex)");
        return Ok(());
    }
    if argv != ["app-server"] {
        return Err("usage: fake-codex app-server".into());
    }
    let delay = Duration::from_millis(
        env::var("FAKE_CODEX_LINE_DELAY_MS").map_or(Ok(0), |v| v.parse::<u64>())?,
    );
    let mut turns: VecDeque<String> = env::var("FAKE_CODEX_TURNS")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let log = match env::var("FAKE_CODEX_LOG") {
        Ok(path) => Some(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?,
        ),
        Err(_) => None,
    };
    let mut fake = Fake {
        out: io::stdout(),
        log,
        thread_id: RECORDED_THREAD.into(),
        cwd: RECORDED_CWD.into(),
        ids: Vec::new(),
        next_server_id: 0,
        turn_id: None,
    };
    let mut script = load("thread_start")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut received: VecDeque<Value> = VecDeque::new();
    let mut next_line = Instant::now();
    loop {
        // Feed the script whatever the client already sent.
        if let Some((dir, expected)) = script.front().cloned() {
            if dir == "server" {
                if Instant::now() >= next_line {
                    script.pop_front();
                    fake.replay(expected)?;
                    next_line = Instant::now() + delay;
                    continue;
                }
            } else if let Some(index) = received.iter().position(|m| matches(&expected, m)) {
                let got = received.remove(index).unwrap();
                script.pop_front();
                if got["method"] == "thread/resume" {
                    fake.thread_id = got["params"]["threadId"]
                        .as_str()
                        .unwrap_or_default()
                        .into();
                }
                if let Some(cwd) = got["params"]["cwd"].as_str() {
                    fake.cwd = cwd.into();
                }
                if let (Some(recorded), Some(live)) = (expected.get("id"), got.get("id")) {
                    fake.ids.push((recorded.clone(), live.clone()));
                }
                continue;
            }
        }
        // Messages the script does not wait for.
        if let Some(index) = received.iter().position(|m| {
            m["method"] == "turn/steer"
                || m["method"] == "turn/interrupt"
                || (m["method"] == "turn/start" && script.is_empty())
        }) {
            let got = received.remove(index).unwrap();
            let id = got["id"].clone();
            match got["method"].as_str() {
                Some("turn/steer") => {
                    fake.send(json!({"id": id, "result": {"turnId": fake.turn_id}}))?
                }
                Some("turn/interrupt") => {
                    fake.send(json!({"id": id, "result": {}}))?;
                    script.clear();
                    fake.send(json!({"method":"thread/status/changed","params":{"threadId":fake.thread_id,"status":{"type":"idle"}}}))?;
                    fake.send(json!({"method":"turn/completed","params":{"threadId":fake.thread_id,"turn":{"id":fake.turn_id,"items":[],"status":"interrupted","error":null}}}))?;
                }
                _ => {
                    let name = turns.pop_front().ok_or("no recorded turn left")?;
                    script = load(&name)?;
                    received.push_front(got);
                }
            }
            continue;
        }
        let wait = match script.front() {
            Some((dir, _)) if dir == "server" => {
                next_line.saturating_duration_since(Instant::now())
            }
            _ => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(line) => {
                let msg: Value = serde_json::from_str(&line?)?;
                fake.record("client", &msg)?;
                received.push_back(msg);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
