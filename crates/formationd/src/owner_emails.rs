//! The owner's email addresses, which answers sent to teammates mask
//! (`OwnerAsks`): Git's global `user.email` and the GitHub account's addresses
//! from `gh`. Whatever cannot be read is skipped.

use std::path::Path;
use std::time::Duration;

use formation_core::peer::{github, process};

/// Each lookup's deadline, so a hung `git` or `gh` cannot hold up startup long.
const TIMEOUT: Duration = Duration::from_secs(3);

/// One lookup: the program (`git` or `gh`) and its arguments.
type Lookup<'a> = (&'a str, &'a [&'a str]);

const GIT: Lookup = ("git", &["config", "--global", "user.email"]);
const GH_EMAILS: Lookup = ("gh", &["api", "user/emails", "--jq", ".[].email"]);
const GH_USER: Lookup = ("gh", &["api", "user", "--jq", ".email"]);

/// Runs every lookup on this Mac. Blocks for up to [`TIMEOUT`] per lookup.
pub fn collect() -> Vec<String> {
    collect_with(run)
}

/// The addresses from Git, then from `gh api user/emails`, or from `gh api user`
/// when that gives none. `run` returns a lookup's standard output, or `None` when
/// it failed; each line that looks like an address counts once.
pub fn collect_with(run: impl Fn(Lookup) -> Option<String>) -> Vec<String> {
    let mut emails: Vec<String> = Vec::new();
    let mut add = |output: Option<String>| {
        let before = emails.len();
        for line in output.iter().flat_map(|output| output.lines()) {
            let email = line.trim();
            let looks_valid = email.contains('@') && !email.contains(char::is_whitespace);
            if looks_valid && !emails.iter().any(|seen| seen.eq_ignore_ascii_case(email)) {
                emails.push(email.to_owned());
            }
        }
        emails.len() > before
    };
    add(run(GIT));
    if !add(run(GH_EMAILS)) {
        add(run(GH_USER));
    }
    emails
}

/// `git` from `PATH` (formationd sets the login shell's), `gh` where Homebrew
/// puts it, as for the GitHub login.
fn run((program, args): Lookup) -> Option<String> {
    let gh;
    let program = match program {
        "gh" => {
            gh = github::find_gh()?;
            gh.as_path()
        }
        other => Path::new(other),
    };
    let env = [
        ("GH_PROMPT_DISABLED", Some("1")),
        ("GH_NO_UPDATE_NOTIFIER", Some("1")),
    ];
    let output = process::run(program, args, &env, TIMEOUT).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lookup table standing in for the commands; a missing entry failed.
    fn collect_from(outputs: &[(Lookup, &str)]) -> Vec<String> {
        collect_with(|lookup| {
            outputs
                .iter()
                .find(|(known, _)| *known == lookup)
                .map(|(_, output)| (*output).to_owned())
        })
    }

    #[test]
    fn git_and_every_github_address_are_collected_once() {
        let emails = collect_from(&[
            (GIT, "dohyeon@example.com\n"),
            (
                GH_EMAILS,
                "Dohyeon@Example.com\n12345+dohyeon@users.noreply.github.com\n",
            ),
            (GH_USER, "unused@example.com\n"),
        ]);
        assert_eq!(
            emails,
            [
                "dohyeon@example.com",
                "12345+dohyeon@users.noreply.github.com"
            ]
        );
    }

    #[test]
    fn the_public_github_address_is_the_fallback() {
        // `user/emails` needs the `user:email` scope, so it often fails.
        let emails = collect_from(&[(GIT, "dohyeon@example.com\n"), (GH_USER, "me@work.dev\n")]);
        assert_eq!(emails, ["dohyeon@example.com", "me@work.dev"]);
        // An empty list counts as nothing found, too.
        let emails = collect_from(&[(GH_EMAILS, "\n"), (GH_USER, "me@work.dev\n")]);
        assert_eq!(emails, ["me@work.dev"]);
    }

    #[test]
    fn whatever_is_unavailable_is_skipped() {
        // No public address: `--jq .email` prints `null`.
        let emails = collect_from(&[(GH_USER, "null\n")]);
        assert!(emails.is_empty(), "{emails:?}");
        assert!(collect_from(&[]).is_empty());
        let emails = collect_from(&[(GIT, ""), (GH_EMAILS, "me@work.dev\n")]);
        assert_eq!(emails, ["me@work.dev"]);
    }
}
