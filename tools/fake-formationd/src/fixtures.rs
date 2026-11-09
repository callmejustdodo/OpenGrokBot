//! Loads and checks the fixtures: worlds (snapshots), scripts (timed events),
//! states (a world plus scripts applied at once), and reactions (what a command
//! returns and which script it plays). Every fixture is decoded into the
//! `formation-proto` type it stands for, so a contract change that breaks one
//! fails `fake-formationd check` and the tests.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use formation_proto::common::Command;
use formation_proto::events::Event;
use formation_proto::snapshot::Snapshot;
use formation_proto::{agent, ask, harness, policy, request, snapshot};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The instant the fixtures are written at: 10:53 on 2026-11-09 in Seoul, M3's
/// "now". Every timestamp moves by the same amount so that this instant becomes
/// the moment the fake starts; "3 min ago" stays 3 minutes ago.
pub const ANCHOR: &str = "2026-11-09T01:53:00Z";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub description: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Wait this long after the previous step when played; ignored when a state
    /// applies the script at once.
    #[serde(default)]
    pub after_ms: u64,
    /// An `Event` (`{ "type", "payload" }`), possibly with placeholders.
    pub event: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub description: String,
    #[serde(default = "default_world")]
    pub world: String,
    /// Applied in order, at once, on top of the world.
    #[serde(default)]
    pub scripts: Vec<String>,
    /// For a state the app shows by itself over this data (a dialog, a sheet, a
    /// draft): what to do in the app to reach it.
    #[serde(default)]
    pub ui: Option<String>,
}

fn default_world() -> String {
    "m3".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reaction {
    /// Params fields this reaction requires, for example `{ "choice": "deny" }`.
    #[serde(default)]
    pub when: Map<String, Value>,
    /// Wait this long before answering, to show a loading state.
    #[serde(default)]
    pub delay_ms: u64,
    /// The command's result, possibly with placeholders.
    pub result: Value,
    /// A script to play after answering.
    #[serde(default)]
    pub play: Option<String>,
    /// Params used to check this reaction against the contract.
    pub example: Value,
}

pub struct Fixtures {
    pub worlds: BTreeMap<String, Value>,
    pub scripts: BTreeMap<String, Script>,
    pub states: BTreeMap<String, State>,
    pub reactions: BTreeMap<String, Vec<Reaction>>,
}

/// The fixtures that ship with the tool.
pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

impl Fixtures {
    /// Reads every fixture and checks it against the contract.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let fixtures = Fixtures {
            worlds: read_dir_json(&dir.join("worlds"))?,
            scripts: read_dir_json(&dir.join("scripts"))?,
            states: read_json(&dir.join("states.json"))?,
            reactions: read_json(&dir.join("reactions.json"))?,
        };
        fixtures.check()?;
        Ok(fixtures)
    }

    fn check(&self) -> Result<(), String> {
        for (name, world) in &self.worlds {
            decode::<Snapshot>(world).map_err(|e| format!("worlds/{name}.json: {e}"))?;
        }
        // A script that a reaction plays is checked with that reaction's example
        // params; any other script must not use params placeholders.
        let mut examples: BTreeMap<&str, &Value> = BTreeMap::new();
        for reaction in self.reactions.values().flatten() {
            if let Some(script) = &reaction.play {
                examples.entry(script.as_str()).or_insert(&reaction.example);
            }
        }
        let no_params = Value::Object(Map::new());
        for (name, script) in &self.scripts {
            let ph =
                Placeholders::new((*examples.get(name.as_str()).unwrap_or(&&no_params)).clone());
            for (i, step) in script.steps.iter().enumerate() {
                decode::<Event>(&ph.fill(&step.event))
                    .map_err(|e| format!("scripts/{name}.json step {}: {e}", i + 1))?;
            }
        }
        for (name, state) in &self.states {
            if !self.worlds.contains_key(&state.world) {
                return Err(format!("states.json {name}: no world {}", state.world));
            }
            if let Some(missing) = state
                .scripts
                .iter()
                .find(|s| !self.scripts.contains_key(*s))
            {
                return Err(format!("states.json {name}: no script {missing}"));
            }
        }
        for (method, reactions) in &self.reactions {
            for (i, reaction) in reactions.iter().enumerate() {
                let at = format!("reactions.json {method}[{i}]");
                let ph = Placeholders::new(reaction.example.clone());
                check_command(method, &reaction.example, &ph.fill(&reaction.result))
                    .map_err(|e| format!("{at}: {e}"))?;
                if let Some(script) = &reaction.play
                    && !self.scripts.contains_key(script)
                {
                    return Err(format!("{at}: no script {script}"));
                }
            }
        }
        Ok(())
    }

    /// The reaction for a call: the first whose `when` fields all match the
    /// params. A `when` key is a params field, or a path such as `target/kind`.
    pub fn reaction(&self, method: &str, params: &Value) -> Option<&Reaction> {
        self.reactions.get(method)?.iter().find(|r| {
            r.when
                .iter()
                .all(|(path, want)| params.pointer(&format!("/{path}")) == Some(want))
        })
    }
}

/// Checks that `params` and `result` decode as the command's params and result.
fn check_command(method: &str, params: &Value, result: &Value) -> Result<(), String> {
    fn both<C: Command>(params: &Value, result: &Value) -> Result<(), String> {
        decode::<C>(params).map_err(|e| format!("example params: {e}"))?;
        decode::<C::Output>(result)
            .map(drop)
            .map_err(|e| format!("result: {e}"))
    }
    macro_rules! dispatch {
        ($($ty:ty),+ $(,)?) => {
            $(if method == <$ty as Command>::METHOD {
                return both::<$ty>(params, result);
            })+
        };
    }
    dispatch!(
        agent::AgentStart,
        agent::AgentStop,
        agent::AgentsDetect,
        harness::AgentMessageSend,
        harness::AgentMessageSteer,
        harness::AgentMessageEdit,
        harness::AgentMessageWithdraw,
        harness::AgentTurnInterrupt,
        ask::Ask,
        ask::QuestionCancel,
        ask::QuestionRefresh,
        ask::QuestionRouteChoose,
        request::RequestCreate,
        request::RequestCancel,
        policy::ApprovalDecide,
        snapshot::SettingsUpdate,
    );
    Err(format!(
        "{method} has no reaction support; add it to check_command"
    ))
}

/// Decodes `value` as `T` and requires that encoding it again gives the same
/// JSON, so a misspelled or extra field fails instead of being dropped or read
/// as `null`.
fn decode<T: DeserializeOwned + Serialize>(value: &Value) -> Result<T, String> {
    let decoded: T = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let again = serde_json::to_value(&decoded).map_err(|e| e.to_string())?;
    match first_difference(value, &again, String::new()) {
        None => Ok(decoded),
        Some(path) => Err(format!(
            "{path}: not part of the contract, or written differently"
        )),
    }
}

/// The JSON path of the first place where `a` and `b` differ.
fn first_difference(a: &Value, b: &Value, path: String) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| match (x.get(k), y.get(k)) {
                (Some(v), Some(w)) => first_difference(v, w, format!("{path}/{k}")),
                _ => Some(format!("{path}/{k}")),
            })
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .enumerate()
            .find_map(|(i, (v, w))| first_difference(v, w, format!("{path}/{i}"))),
        // f32 fields such as a routing score come back with float noise.
        (Value::Number(x), Value::Number(y))
            if (x.as_f64().unwrap_or(f64::NAN) - y.as_f64().unwrap_or(f64::NAN)).abs() < 1e-6 =>
        {
            None
        }
        _ if a == b => None,
        _ => Some(if path.is_empty() { "/".into() } else { path }),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_dir_json<T: DeserializeOwned>(dir: &Path) -> Result<BTreeMap<String, T>, String> {
    let mut out = BTreeMap::new();
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("{}: bad file name", path.display()))?
                .to_owned();
            out.insert(name, read_json(&path)?);
        }
    }
    Ok(out)
}

/// Moves every RFC 3339 timestamp string in `value` by `by`.
pub fn shift_times(value: &mut Value, by: TimeDelta) {
    match value {
        Value::String(s) => {
            if let Ok(t) = DateTime::parse_from_rfc3339(s) {
                *s = (t.with_timezone(&Utc) + by).to_rfc3339_opts(SecondsFormat::Secs, true);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| shift_times(v, by)),
        Value::Object(map) => map.values_mut().for_each(|v| shift_times(v, by)),
        _ => {}
    }
}

/// How far fixture times move so that [`ANCHOR`] becomes `now`.
pub fn shift_to(now: DateTime<Utc>) -> TimeDelta {
    let anchor: DateTime<Utc> = ANCHOR.parse().expect("ANCHOR is RFC 3339");
    now - anchor
}

/// Fills placeholders in fixture strings:
///
/// - `{{params.<field>}}`: from the command's params (`.` for nested fields);
/// - `{{id:<name>}}`: a fresh UUIDv7 per name, shared by a reaction's result and
///   the script it plays;
/// - `{{now}}`: the current time, written in fixture time so that the shift to
///   real time lands on now.
///
/// A string that is exactly one placeholder becomes the value itself (any JSON
/// type); a placeholder inside longer text is replaced by the value as text.
pub struct Placeholders {
    params: Value,
    now: String,
    ids: std::cell::RefCell<BTreeMap<String, String>>,
}

impl Placeholders {
    /// For checking fixtures: `{{now}}` is the anchor.
    pub fn new(params: Value) -> Self {
        Self::at(params, ANCHOR.to_owned())
    }

    /// `now` in fixture time (real now minus the shift).
    pub fn at(params: Value, now: String) -> Self {
        Self {
            params,
            now,
            ids: Default::default(),
        }
    }

    pub fn fill(&self, value: &Value) -> Value {
        match value {
            Value::String(s) => self.fill_str(s),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.fill(v)).collect()),
            Value::Object(map) => {
                Value::Object(map.iter().map(|(k, v)| (k.clone(), self.fill(v))).collect())
            }
            other => other.clone(),
        }
    }

    fn fill_str(&self, s: &str) -> Value {
        if let Some(inner) = s.strip_prefix("{{").and_then(|r| r.strip_suffix("}}"))
            && !inner.contains("{{")
        {
            return self.lookup(inner);
        }
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("{{") {
            let Some(len) = rest[start..].find("}}") else {
                break;
            };
            out.push_str(&rest[..start]);
            match self.lookup(&rest[start + 2..start + len]) {
                Value::String(v) => out.push_str(&v),
                other => out.push_str(&other.to_string()),
            }
            rest = &rest[start + len + 2..];
        }
        out.push_str(rest);
        Value::String(out)
    }

    fn lookup(&self, key: &str) -> Value {
        if key == "now" {
            return Value::String(self.now.clone());
        }
        if let Some(name) = key.strip_prefix("id:") {
            let mut ids = self.ids.borrow_mut();
            let id = ids
                .entry(name.to_owned())
                .or_insert_with(|| uuid::Uuid::now_v7().to_string());
            return Value::String(id.clone());
        }
        if let Some(field) = key.strip_prefix("params.") {
            return self
                .params
                .pointer(&format!("/{}", field.replace('.', "/")))
                .cloned()
                .unwrap_or(Value::Null);
        }
        Value::String(format!("{{{{{key}}}}}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_shipped_fixtures_match_the_contract() {
        Fixtures::load(&default_dir()).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn every_required_m3_state_is_named() {
        // TASK-043 acceptance: every M3 state of these screens.
        let required = [
            ("SC-003", &["default", "empty", "detecting"][..]),
            (
                "SC-005",
                &[
                    "working",
                    "idle",
                    "queued",
                    "permission",
                    "approval",
                    "stalled",
                    "error",
                    "stopped",
                    "detected",
                    "stop-confirm",
                ],
            ),
            ("SC-010", &["default", "empty"]),
            (
                "SC-011",
                &[
                    "default",
                    "keep-similar",
                    "expiring",
                    "expired",
                    "deny-confirm",
                ],
            ),
            (
                "SC-015",
                &[
                    "answered",
                    "new",
                    "received",
                    "answering",
                    "unsure",
                    "stale",
                    "offline-waiting",
                    "sent-to-owner",
                    "unshared",
                    "error",
                ],
            ),
            ("SC-016", &["compose", "status", "not-accepted"]),
        ];
        let fixtures = Fixtures::load(&default_dir()).unwrap();
        for (screen, states) in required {
            for state in states {
                let name = format!("{screen}/{state}");
                assert!(fixtures.states.contains_key(&name), "missing state {name}");
            }
        }
    }

    #[test]
    fn placeholders_fill_whole_values_and_text() {
        let ph = Placeholders::new(json!({ "text": "hi", "n": 3 }));
        let out = ph.fill(&json!({
            "a": "{{params.n}}",
            "b": "said: {{params.text}}",
            "c": "{{id:x}}",
            "d": "{{id:x}}",
        }));
        assert_eq!(out["a"], 3);
        assert_eq!(out["b"], "said: hi");
        assert_eq!(out["c"], out["d"]);
        assert!(uuid::Uuid::parse_str(out["c"].as_str().unwrap()).is_ok());
    }

    #[test]
    fn times_move_and_other_strings_stay() {
        let mut v = json!({ "at": "2026-11-09T01:53:00Z", "text": "10:53" });
        shift_times(&mut v, TimeDelta::hours(1));
        assert_eq!(v["at"], "2026-11-09T02:53:00Z");
        assert_eq!(v["text"], "10:53");
    }
}
