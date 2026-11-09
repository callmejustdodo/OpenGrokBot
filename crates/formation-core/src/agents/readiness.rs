use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::ports::Result;
use formation_proto::agent::{HarnessKind, HarnessStatus};

/// Problems from a failed authentication, by located Claude Code executable. They
/// override `claude auth status` until a turn succeeds: in the IC-1 run it still said
/// signed in after the OAuth session had expired. Process-wide, so the supervisor's
/// `status` and the `harness_check` command see the same state.
static AUTH_PROBLEMS: Mutex<Option<HashMap<PathBuf, String>>> = Mutex::new(None);

/// Inject readiness in tests without requiring an installed or signed-in harness.
pub trait HarnessReadiness: Send + Sync {
    fn check(&self) -> Result<Vec<HarnessStatus>>;
}

/// Claude Code's own CLI reports sign-in state; credentials are never read.
/// `FORMATION_CLAUDE_BIN`, otherwise `claude` on PATH, matches the TASK-004 spike.
/// Codex readiness is deferred to the Codex adapter task.
pub struct ClaudeReadiness {
    binary: PathBuf,
}
impl Default for ClaudeReadiness {
    fn default() -> Self {
        Self::new(
            std::env::var_os("FORMATION_CLAUDE_BIN")
                .unwrap_or_else(|| "claude".into())
                .into(),
        )
    }
}
impl ClaudeReadiness {
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }
    /// Records (`Some`) or clears (`None`) an authentication failure of `binary`;
    /// returns whether the recorded state changed.
    pub fn set_auth_problem(binary: &Path, problem: Option<String>) -> bool {
        let key = locate(binary).unwrap_or_else(|| binary.to_path_buf());
        let mut problems = AUTH_PROBLEMS.lock().unwrap();
        let problems = problems.get_or_insert_with(HashMap::new);
        match problem {
            Some(problem) => problems.insert(key, problem.clone()).as_ref() != Some(&problem),
            None => problems.remove(&key).is_some(),
        }
    }
}
impl HarnessReadiness for ClaudeReadiness {
    fn check(&self) -> Result<Vec<HarnessStatus>> {
        let path = locate(&self.binary);
        let mut status = HarnessStatus {
            harness: HarnessKind::ClaudeCode,
            installed: path.is_some(),
            signed_in: None,
            version: None,
            path: path.clone(),
            problem: None,
        };
        let Some(path) = path else {
            status.problem = Some("Install Claude Code or locate its executable".into());
            return Ok(vec![status]);
        };
        status.version = run(&path, &["--version"])
            .filter(|(ok, _)| *ok)
            .and_then(|(_, output)| String::from_utf8(output).ok())
            .map(|s| s.trim().to_owned());
        status.signed_in = run(&path, &["auth", "status", "--json"]).and_then(|(ok, output)| {
            let value = serde_json::from_slice::<serde_json::Value>(&output).ok()?;
            let signed_in = value.get("loggedIn")?.as_bool()?;
            (ok || !signed_in).then_some(signed_in)
        });
        status.problem = match status.signed_in {
            Some(true) => None,
            Some(false) => Some("Sign in to Claude Code with claude auth login".into()),
            None => Some("Could not determine Claude Code sign-in status".into()),
        };
        if let Some(problem) = AUTH_PROBLEMS
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|problems| problems.get(&path))
        {
            status.signed_in = Some(false);
            status.problem = Some(problem.clone());
        }
        Ok(vec![status])
    }
}

fn locate(binary: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &Path| {
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if binary.components().count() > 1 || binary.is_absolute() {
        return executable(binary)
            .then(|| binary.canonicalize().ok())
            .flatten();
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(binary))
        .find(|path| executable(path))
        .and_then(|path| path.canonicalize().ok())
}

fn run(binary: &Path, args: &[&str]) -> Option<(bool, Vec<u8>)> {
    let output = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some((output.status.success(), output.stdout))
}
