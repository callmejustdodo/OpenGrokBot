//! Where Claude Code keeps a session's transcript:
//! `<config dir>/projects/<project dir name>/<session-id>.jsonl`.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// Encoded names longer than this many characters are shortened by the CLI.
const MAX_ENCODED_LEN: usize = 200;

/// Claude Code's configuration directory: `$CLAUDE_CONFIG_DIR` if set,
/// otherwise `~/.claude`.
pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(dir.into());
    }
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".claude"))
}

/// The project folder name Claude Code derives from a working directory's
/// real path, following the CLI's own code (Claude Code 2.1.319):
///
/// 1. The path is normalized to Unicode NFC. macOS can store folder names
///    decomposed (NFD), where a Korean syllable is two or three characters.
/// 2. Every UTF-16 unit outside `[A-Za-z0-9]` becomes `-`, so `/-` becomes
///    `--` and each Korean syllable becomes one `-`.
/// 3. A result longer than 200 characters is cut to 200 and followed by `-`
///    and the base-36 absolute value of the Java-style 32-bit hash of the
///    NFC path's UTF-16 units.
///
/// It does not apply `CLAUDE_CODE_PROJECT_DIR_NAME`; [`project_dir`] does.
pub fn project_dir_name(real_path: &Path) -> String {
    let path: String = real_path.to_string_lossy().nfc().collect();
    let mut name = String::new();
    for c in path.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if name.len() <= MAX_ENCODED_LEN {
        return name;
    }
    // `name` is ASCII, so byte and character positions agree.
    name.truncate(MAX_ENCODED_LEN);
    name.push('-');
    name.push_str(&base36(java_hash(path.encode_utf16()).unsigned_abs()));
    name
}

/// The project folder that a session started in `agent_dir` writes to.
/// `agent_dir` must exist: the CLI records the real path, so `/tmp/x`
/// resolves to `/private/tmp/x` on macOS.
///
/// Like the CLI, it uses `$CLAUDE_CODE_PROJECT_DIR_NAME` instead of the
/// derived name when `$CLAUDE_CONFIG_DIR` is set and the name is valid.
pub fn project_dir(config_dir: &Path, agent_dir: &Path) -> io::Result<PathBuf> {
    let name = match project_dir_override(
        env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        env::var_os("CLAUDE_CODE_PROJECT_DIR_NAME").as_deref(),
    ) {
        Some(name) => name,
        None => project_dir_name(&fs::canonicalize(agent_dir)?),
    };
    Ok(config_dir.join("projects").join(name))
}

/// The path where the transcript of `session_id`, started in `agent_dir`,
/// is expected. See [`project_dir`].
pub fn transcript_path(
    config_dir: &Path,
    agent_dir: &Path,
    session_id: &str,
) -> io::Result<PathBuf> {
    check_session_id(session_id)?;
    Ok(project_dir(config_dir, agent_dir)?.join(format!("{session_id}.jsonl")))
}

/// Finds the transcript of `session_id`: the expected path first, otherwise
/// any project folder that holds `<session_id>.jsonl`.
pub fn find_transcript(
    config_dir: &Path,
    agent_dir: &Path,
    session_id: &str,
) -> io::Result<Option<PathBuf>> {
    let expected = transcript_path(config_dir, agent_dir, session_id)?;
    if expected.is_file() {
        return Ok(Some(expected));
    }
    let projects = match fs::read_dir(config_dir.join("projects")) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let file_name = format!("{session_id}.jsonl");
    for entry in projects {
        let candidate = entry?.path().join(&file_name);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// The CLI's rule for `CLAUDE_CODE_PROJECT_DIR_NAME`: used only when
/// `CLAUDE_CONFIG_DIR` is non-empty, and only if it is 1 to 64 characters
/// from `[A-Za-z0-9_-]` and not a reserved Windows device name.
fn project_dir_override(config_dir: Option<&OsStr>, name: Option<&OsStr>) -> Option<String> {
    config_dir.filter(|dir| !dir.is_empty())?;
    let name = name?.to_str()?;
    let valid = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !is_windows_device_name(name);
    valid.then(|| name.to_owned())
}

/// `con`, `prn`, `aux`, `nul`, `com0`-`com9` and `lpt0`-`lpt9`, in any case.
fn is_windows_device_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(lower.as_str(), "con" | "prn" | "aux" | "nul")
        || (lower.len() == 4
            && (lower.starts_with("com") || lower.starts_with("lpt"))
            && lower.as_bytes()[3].is_ascii_digit())
}

/// JavaScript's `h = (h << 5) - h + unit | 0` over UTF-16 units.
fn java_hash(units: impl IntoIterator<Item = u16>) -> i32 {
    units.into_iter().fold(0i32, |hash, unit| {
        (hash << 5).wrapping_sub(hash).wrapping_add(i32::from(unit))
    })
}

/// Lowercase base 36, as JavaScript's `Number.prototype.toString(36)`.
fn base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).expect("base-36 digits are ASCII")
}

/// Session IDs become file names, so only letters, digits and `-` pass.
fn check_session_id(session_id: &str) -> io::Result<()> {
    let valid = !session_id.is_empty()
        && !session_id.starts_with('-')
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a session ID: {session_id:?}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::test_support::TempDir;

    const SID: &str = "11111111-2222-4333-8444-555555555555";

    #[test]
    fn every_non_alphanumeric_character_becomes_a_dash() {
        assert_eq!(
            project_dir_name(Path::new(
                "/private/tmp/claude-502/-Users-me-dev-Shared/scratchpad/agent"
            )),
            "-private-tmp-claude-502--Users-me-dev-Shared-scratchpad-agent"
        );
        assert_eq!(project_dir_name(Path::new("/a b/c.d_e")), "-a-b-c-d-e");
    }

    #[test]
    fn non_ascii_characters_become_one_dash_per_utf16_unit() {
        assert_eq!(
            project_dir_name(Path::new("/Users/me/바탕화면")),
            "-Users-me-----"
        );
        assert_eq!(project_dir_name(Path::new("/x/🦀")), "-x---");
    }

    // The expected names below come from running the CLI's own functions
    // (copied from the Claude Code 2.1.319 bundle) under Node.

    #[test]
    fn decomposed_korean_names_are_normalized_first() {
        let nfd: String = "/Users/me/바탕화면".nfd().collect();
        assert_eq!(nfd.chars().count(), 20, "the input is decomposed");
        assert_eq!(project_dir_name(Path::new(&nfd)), "-Users-me-----");
    }

    #[test]
    fn names_over_200_characters_are_cut_and_hashed() {
        let long = format!("/private/tmp/{}", "x".repeat(190));
        let expected = format!("-private-tmp-{}-q3zib7", "x".repeat(187));
        assert_eq!(project_dir_name(Path::new(&long)), expected);

        // The hash covers the whole NFC path, Korean included, and a
        // negative hash uses its absolute value.
        let long = format!("/Users/me/{}프로젝트", "deep/".repeat(40));
        let expected = format!("-Users-me-{}-wb62q4", "deep-".repeat(38));
        assert_eq!(project_dir_name(Path::new(&long)), expected);
        assert_eq!(java_hash(long.encode_utf16()), -1_953_676_876);
    }

    #[test]
    fn a_name_of_exactly_200_characters_is_kept() {
        let path = format!("/{}", "a".repeat(199));
        assert_eq!(project_dir_name(Path::new(&path)), path.replace('/', "-"));
    }

    #[test]
    fn base36_matches_javascript() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(1_578_816_115), "q3zib7");
        assert_eq!(base36(i32::MIN.unsigned_abs()), "zik0zk");
    }

    #[test]
    fn project_dir_name_override_needs_a_config_dir_and_a_valid_name() {
        let dir = Some(OsStr::new("/cfg"));
        let ok = Some(OsStr::new("team_agent-1"));
        assert_eq!(
            project_dir_override(dir, ok).as_deref(),
            Some("team_agent-1")
        );
        assert_eq!(project_dir_override(None, ok), None);
        assert_eq!(project_dir_override(Some(OsStr::new("")), ok), None);
        assert_eq!(project_dir_override(dir, None), None);
        let too_long = "a".repeat(65);
        for bad in ["", "a/b", "..", "a b", "CON", "lpt1", too_long.as_str()] {
            assert_eq!(
                project_dir_override(dir, Some(OsStr::new(bad))),
                None,
                "{bad:?}"
            );
        }
        assert_eq!(
            project_dir_override(dir, Some(OsStr::new("com"))).as_deref(),
            Some("com")
        );
    }

    #[test]
    fn transcript_path_uses_the_real_agent_path() {
        let config = TempDir::new("ask-config");
        let agent = TempDir::new("ask-agent");
        let path = transcript_path(config.path(), agent.path(), SID).unwrap();
        let real = fs::canonicalize(agent.path()).unwrap();
        assert_eq!(
            path,
            config
                .path()
                .join("projects")
                .join(project_dir_name(&real))
                .join(format!("{SID}.jsonl"))
        );
    }

    #[test]
    fn session_ids_that_could_escape_the_folder_are_rejected() {
        let config = TempDir::new("ask-config");
        let agent = TempDir::new("ask-agent");
        for bad in ["", "../x", "a/b", "-r", "a.jsonl"] {
            let err = transcript_path(config.path(), agent.path(), bad).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad:?}");
        }
    }

    #[test]
    fn find_transcript_prefers_the_expected_path_then_searches() {
        let config = TempDir::new("ask-config");
        let agent = TempDir::new("ask-agent");
        assert_eq!(
            find_transcript(config.path(), agent.path(), SID).unwrap(),
            None
        );

        let elsewhere = config.path().join("projects").join("shortened-name");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join(format!("{SID}.jsonl")), "{}\n").unwrap();
        assert_eq!(
            find_transcript(config.path(), agent.path(), SID).unwrap(),
            Some(elsewhere.join(format!("{SID}.jsonl")))
        );

        let expected = transcript_path(config.path(), agent.path(), SID).unwrap();
        fs::create_dir_all(expected.parent().unwrap()).unwrap();
        fs::write(&expected, "{}\n").unwrap();
        assert_eq!(
            find_transcript(config.path(), agent.path(), SID).unwrap(),
            Some(expected)
        );
    }
}
