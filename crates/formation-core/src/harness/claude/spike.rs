//! TASK-004 spike: drives the real `claude` in stream-json mode and records what it does.
//!
//! This is a measuring instrument for `tests/claude_spike.rs`, not the TASK-013 supervisor.

use serde_json::{Map, Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// How the process attaches to its conversation.
pub enum Launch<'a> {
    New(&'a str),
    Resume(&'a str),
}

/// The binary under test: `FORMATION_CLAUDE_BIN`, or `claude` from `PATH`.
pub fn claude_bin() -> String {
    std::env::var("FORMATION_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string())
}

/// A fresh agent folder outside the repository, from `mktemp -d`.
pub fn fresh_folder() -> PathBuf {
    let output = Command::new("mktemp")
        .arg("-d")
        .output()
        .expect("run mktemp");
    PathBuf::from(String::from_utf8(output.stdout).expect("utf-8 path").trim())
}

/// One running `claude` process with everything it read and wrote.
pub struct Session {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<(u128, String)>,
    started: Instant,
    /// Parsed stdout lines; `events[i]` is line `i + 1` of the stdout fixture.
    pub events: Vec<Value>,
    stdout_log: Vec<String>,
    stdin_log: Vec<String>,
    timeline: Vec<(u128, String)>,
}

impl Session {
    pub fn start(folder: &Path, launch: Launch, allowed_tools: &[&str]) -> Self {
        let mut command = Command::new(claude_bin());
        command.current_dir(folder).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--replay-user-messages",
        ]);
        match launch {
            Launch::New(id) => command.args(["--session-id", id]),
            Launch::Resume(id) => command.args(["--resume", id]),
        };
        if !allowed_tools.is_empty() {
            command.arg("--allowedTools").args(allowed_tools);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn claude");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let started = Instant::now();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send((started.elapsed().as_millis(), line)).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            started,
            events: Vec::new(),
            stdout_log: Vec::new(),
            stdin_log: Vec::new(),
            timeline: Vec::new(),
        }
    }

    /// Writes one JSON line to stdin.
    pub fn send(&mut self, message: Value) {
        let line = message.to_string();
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").expect("write stdin");
        stdin.flush().expect("flush stdin");
        let at = self.started.elapsed().as_millis();
        self.stdin_log.push(line);
        self.timeline.push((
            at,
            format!(
                "stdin#{:<3} {}",
                self.stdin_log.len(),
                summarize(&message, usize::MAX)
            ),
        ));
    }

    /// Sends a user message, adding `extra` fields such as `priority`.
    pub fn send_user(&mut self, text: &str, extra: Value) {
        let mut message = json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "parent_tool_use_id": null,
        });
        if let (Some(target), Value::Object(fields)) = (message.as_object_mut(), extra) {
            target.extend(fields);
        }
        self.send(message);
    }

    /// Waits for the first event at or after `from` that matches; returns its index.
    /// `None` means the timeout passed or stdout closed first.
    pub fn wait_for(
        &mut self,
        from: usize,
        timeout: Duration,
        matches: impl Fn(&Value) -> bool,
    ) -> Option<usize> {
        let deadline = Instant::now() + timeout;
        let mut index = from;
        loop {
            while index < self.events.len() {
                if matches(&self.events[index]) {
                    return Some(index);
                }
                index += 1;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok((at, line)) => self.record(at, line),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Reads stdout for `duration`, or until it closes.
    pub fn drain(&mut self, duration: Duration) {
        let from = self.events.len();
        self.wait_for(from, duration, |_| false);
    }

    fn record(&mut self, at: u128, line: String) {
        let event: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        self.events.push(event);
        let number = self.events.len();
        self.timeline.push((
            at,
            format!(
                "stdout#{number:<3} {}",
                summarize(&self.events[number - 1], 60)
            ),
        ));
        self.stdout_log.push(line);
    }

    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    pub fn sigint(&mut self) {
        let status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -INT failed");
        let at = self.started.elapsed().as_millis();
        self.timeline.push((at, "signal    SIGINT".to_string()));
    }

    /// Reads stdout until the process exits; `None` if it is still running at the deadline.
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain(Duration::from_millis(100));
            if let Some(status) = self.child.try_wait().expect("poll child") {
                self.drain(Duration::from_millis(200));
                let at = self.started.elapsed().as_millis();
                self.timeline.push((at, format!("exit      {status}")));
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().expect("poll child").is_none()
    }

    /// Writes `<name>.stdin.jsonl`, `<name>.stdout.jsonl` and `<name>.timeline.txt`, scrubbed.
    pub fn save(&self, folder: &Path, name: &str) {
        fs::create_dir_all(folder).expect("create fixture folder");
        let home = std::env::var("HOME").expect("HOME is set");
        let write = |suffix: &str, lines: &[String]| {
            let body: String = lines.iter().map(|line| scrub(line, &home) + "\n").collect();
            fs::write(folder.join(format!("{name}.{suffix}")), body).expect("write fixture");
        };
        write("stdin.jsonl", &self.stdin_log);
        write("stdout.jsonl", &self.stdout_log);
        let mut timeline = self.timeline.clone();
        timeline.sort_by_key(|(at, _)| *at);
        let timeline: Vec<String> = timeline
            .iter()
            .map(|(at, text)| format!("{at:>7}ms {text}"))
            .collect();
        write("timeline.txt", &timeline);
        println!("--- {name} ---\n{}", timeline.join("\n"));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One-line description of an event for the timeline; text is cut at `limit` characters.
fn summarize(event: &Value, limit: usize) -> String {
    let kind = event["type"].as_str().unwrap_or("non-json");
    let mut parts = vec![kind.to_string()];
    for field in [&event["subtype"], &event["event"]["type"]] {
        if let Some(text) = field.as_str() {
            parts.push(text.to_string());
        }
    }
    if let Some(blocks) = event["message"]["content"].as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use") => parts.push(format!("tool_use:{}", block["name"])),
                Some("tool_result") => parts.push("tool_result".to_string()),
                _ => {}
            }
        }
    }
    let text = result_text(event)
        .or_else(|| message_text(event))
        .unwrap_or_default();
    if !text.is_empty() {
        parts.push(format!(
            "{:?}",
            text.chars().take(limit).collect::<String>()
        ));
    }
    parts.join(" ")
}

pub fn is_init(event: &Value) -> bool {
    event["type"] == "system" && event["subtype"] == "init"
}

pub fn is_result(event: &Value) -> bool {
    event["type"] == "result"
}

pub fn result_text(event: &Value) -> Option<String> {
    event["result"]
        .as_str()
        .filter(|_| is_result(event))
        .map(str::to_string)
}

/// The text blocks of a user or assistant message, joined.
pub fn message_text(event: &Value) -> Option<String> {
    match &event["message"]["content"] {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// A `user` event whose own text contains `marker`, such as a replayed message.
pub fn is_user_text(event: &Value, marker: &str) -> bool {
    event["type"] == "user" && message_text(event).is_some_and(|text| text.contains(marker))
}

pub fn is_bash_call(event: &Value) -> bool {
    event["type"] == "assistant"
        && event["message"]["content"]
            .as_array()
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block["type"] == "tool_use" && block["name"] == "Bash")
            })
}

/// The transcript of `session_id` under `CLAUDE_CONFIG_DIR` (or `~/.claude`), if present.
pub fn find_transcript(session_id: &str) -> Option<PathBuf> {
    let root = std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").expect("HOME is set")).join(".claude")
        });
    fs::read_dir(root.join("projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|project| project.path().join(format!("{session_id}.jsonl")))
        .find(|path| path.exists())
}

/// Removes the home directory, user name, emails, and account or credential values.
pub fn scrub(line: &str, home: &str) -> String {
    let user = home.rsplit('/').next().unwrap_or_default();
    let text = line.replace(home, "/Users/USER").replace(user, "USER");
    let text = mask_emails(&text);
    match serde_json::from_str::<Value>(&text) {
        Ok(mut value) => {
            if value["type"] == "system" && value["subtype"] == "hook_response" {
                for field in ["output", "stdout", "stderr"] {
                    value[field] = json!("");
                }
            }
            scrub_value(&mut value);
            value.to_string()
        }
        Err(_) => text,
    }
}

fn scrub_value(value: &mut Value) {
    match value {
        Value::Object(fields) => scrub_fields(fields),
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        _ => {}
    }
}

fn scrub_fields(fields: &mut Map<String, Value>) {
    const SENSITIVE: [&str; 9] = [
        "email",
        "account",
        "organization",
        "org_id",
        "token",
        "api_key",
        "secret",
        "credential",
        "overage",
    ];
    // What this machine has installed or connected, listed by the init event.
    const LOCAL_LISTS: [&str; 6] = [
        "mcp_servers",
        "plugins",
        "agents",
        "skills",
        "slash_commands",
        "terminal_slash_commands",
    ];
    for (key, value) in fields.iter_mut() {
        let key = key.to_lowercase();
        if LOCAL_LISTS.contains(&key.as_str()) && value.is_array() {
            *value = json!([]);
        } else if key.contains("credential") && value.is_number() {
            *value = json!(0);
        } else if value.is_string() && SENSITIVE.iter().any(|word| key.contains(word)) {
            *value = Value::String("REDACTED".to_string());
        } else {
            scrub_value(value);
        }
    }
}

fn mask_emails(text: &str) -> String {
    let bytes = text.as_bytes();
    let is_local = |b: u8| b.is_ascii_alphanumeric() || b"._%+-".contains(&b);
    let is_domain = |b: u8| b.is_ascii_alphanumeric() || b".-".contains(&b);
    let mut masked = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices('@') {
        if at < copied {
            continue;
        }
        let mut start = at;
        while start > copied && is_local(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at + 1;
        while end < bytes.len() && is_domain(bytes[end]) {
            end += 1;
        }
        if start < at && text[at + 1..end].contains('.') {
            masked.push_str(&text[copied..start]);
            masked.push_str("user@example.invalid");
            copied = end;
        }
    }
    masked.push_str(&text[copied..]);
    masked
}
