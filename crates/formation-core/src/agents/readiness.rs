use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ports::Result;
use formation_proto::agent::{HarnessKind, HarnessStatus};

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
