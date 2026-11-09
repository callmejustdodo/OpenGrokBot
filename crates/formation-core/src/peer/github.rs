//! The local GitHub login from an existing `gh` session, the MVP fallback for
//! GitHub identity (backend architecture, "Identity and team"). It proves the
//! login only to this Mac; how the creator's Mac checks it is TASK-017's job.

use std::path::PathBuf;
use std::time::Duration;

use super::process;

/// Homebrew on Apple silicon, then Homebrew on Intel (or a manual install).
pub const GH_CANDIDATES: [&str; 2] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh"];

const GH_TIMEOUT: Duration = Duration::from_secs(10);

pub fn find_gh() -> Option<PathBuf> {
    process::first_executable(GH_CANDIDATES)
}

/// `gh api user --jq .login`.
pub fn github_login() -> Result<String, String> {
    let gh = find_gh().ok_or_else(|| format!("gh not found at {}", GH_CANDIDATES.join(" or ")))?;
    // Without HOME, gh cannot find its config and writes state into the
    // current directory instead.
    if std::env::var_os("HOME").is_none() {
        return Err("HOME is not set, so gh cannot find its session".into());
    }
    let output = process::run(
        &gh,
        &["api", "user", "--jq", ".login"],
        &[
            ("GH_PROMPT_DISABLED", Some("1")),
            ("GH_NO_UPDATE_NOTIFIER", Some("1")),
        ],
        GH_TIMEOUT,
    )
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "gh api user failed ({}): {}",
            output.status,
            output.first_line()
        ));
    }
    parse_github_login(&output.stdout)
        .ok_or_else(|| format!("gh printed no GitHub login: {:?}", output.first_line()))
}

/// A GitHub login: 1 to 39 ASCII letters, digits, or hyphens, not starting with
/// a hyphen.
pub fn parse_github_login(stdout: &[u8]) -> Option<String> {
    let login = std::str::from_utf8(stdout).ok()?.trim();
    let valid = (1..=39).contains(&login.len())
        && !login.starts_with('-')
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    valid.then(|| login.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_github_logins() {
        assert_eq!(parse_github_login(b"octo-cat\n"), Some("octo-cat".into()));
        for bad in [&b""[..], b"\n", b"-octo", b"octo cat", b"octo@example.com"] {
            assert_eq!(
                parse_github_login(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(parse_github_login(&[b'a'; 40]), None);
    }
}
