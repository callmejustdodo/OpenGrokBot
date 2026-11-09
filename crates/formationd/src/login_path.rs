//! A LaunchAgent starts formationd with macOS's minimal PATH, which misses where
//! `claude`, `codex`, `gh` and `tailscale` are usually installed. formationd puts the
//! owner's login-shell PATH first, then its own PATH, then well-known install folders.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where the first part of the effective PATH came from, for the startup log.
pub enum Source {
    LoginShell(PathBuf),
    Fallback,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::LoginShell(shell) => write!(f, "login-shell({})", shell.display()),
            Source::Fallback => f.write_str("fallback"),
        }
    }
}

/// Builds the effective PATH from the real environment.
pub fn resolve() -> (OsString, Source) {
    let shell = std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map_or_else(|| PathBuf::from("/bin/zsh"), PathBuf::from);
    let login = login_shell_path(&shell, Duration::from_secs(3));
    let source = match login {
        Some(_) => Source::LoginShell(shell),
        None => Source::Fallback,
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = merge(
        login,
        std::env::var_os("PATH"),
        &well_known(home.as_deref()),
    );
    (path, source)
}

/// The PATH printed by `shell -lc`, or `None` when the shell fails, prints nothing
/// or does not finish within `timeout`.
pub fn login_shell_path(shell: &Path, timeout: Duration) -> Option<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut child = Command::new(shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut output = Vec::new();
    child.stdout.take()?.read_to_end(&mut output).ok()?;
    // Startup files may print before the PATH; `printf %s` writes it last, unterminated.
    let path = output.rsplit(|b| *b == b'\n').next()?.to_vec();
    (status.success() && !path.is_empty()).then(|| OsString::from_vec(path))
}

fn well_known(home: Option<&Path>) -> Vec<PathBuf> {
    let mut folders = Vec::new();
    if let Some(home) = home {
        folders.push(home.join(".local/bin"));
    }
    folders.push("/opt/homebrew/bin".into());
    folders.push("/usr/local/bin".into());
    if let Some(home) = home {
        folders.push(home.join(".claude/local"));
        folders.push(home.join(".npm-global/bin"));
    }
    folders
}

/// `login`, then `current`, then the `extra` folders that exist, without duplicates
/// or empty entries, in that order.
pub fn merge(login: Option<OsString>, current: Option<OsString>, extra: &[PathBuf]) -> OsString {
    let mut entries: Vec<PathBuf> = Vec::new();
    let listed = [login, current]
        .into_iter()
        .flatten()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>());
    let existing = extra.iter().filter(|p| p.is_dir()).cloned();
    for entry in listed.chain(existing) {
        if !entry.as_os_str().is_empty() && !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    std::env::join_paths(entries).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_order_drops_duplicates_and_skips_missing_folders() {
        let root = std::env::temp_dir().join(format!("formationd-path-{}", std::process::id()));
        let present = root.join("present");
        std::fs::create_dir_all(&present).unwrap();
        let missing = root.join("missing");
        let path = merge(
            Some("/login/a:/shared:/login/b".into()),
            Some("/usr/bin:/shared:/login/a:/bin".into()),
            &[missing, present.clone(), "/bin".into()],
        );
        let expected = format!(
            "/login/a:/shared:/login/b:/usr/bin:/bin:{}",
            present.display()
        );
        assert_eq!(path, OsString::from(expected));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shell_failure_falls_back_to_current_path() {
        let shell = Path::new("/nonexistent/formation-shell");
        let login = login_shell_path(shell, Duration::from_secs(1));
        assert!(login.is_none());
        assert!(login_shell_path(Path::new("/usr/bin/false"), Duration::from_secs(1)).is_none());
        let path = merge(login, Some("/usr/bin:/bin".into()), &[]);
        assert_eq!(path, OsString::from("/usr/bin:/bin"));
    }
}
