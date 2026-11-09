//! Registers `formationd` as a per-user LaunchAgent, so it keeps running when
//! the app quits and launchd restarts it if it exits (D-048, TASK-009).
//!
//! The app writes `~/Library/LaunchAgents/<label>.plist` and bootstraps it in
//! the `gui/<uid>` domain with `launchctl`. SMAppService would need the app
//! bundle, and the team runs unbundled dev builds (`tauri dev`), so the plist
//! route is used for both dev and bundled builds.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Name of the service binary, which sits next to the app's own executable:
/// `target/<profile>/` in development and `Contents/MacOS/` in the bundle.
pub const PROGRAM_NAME: &str = "formationd";

/// The LaunchAgent label for an app identifier.
pub fn label(app_identifier: &str) -> String {
    format!("{app_identifier}.{PROGRAM_NAME}")
}

/// The LaunchAgent property list for `program`, with stdout and stderr in `log`.
pub fn render_plist(label: &str, program: &Path, log: &Path) -> String {
    let label = xml_escape(label);
    let program = xml_escape(&program.to_string_lossy());
    let log = xml_escape(&log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{program}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// What registration has to do, given the plist on disk and launchd's state.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// The same plist is loaded; leave the running formationd alone.
    KeepRunning,
    /// Write the plist and bootstrap it. `replace` boots out the loaded copy
    /// first, which restarts formationd (and, later, every agent it runs).
    Bootstrap { replace: bool },
}

pub fn plan(existing: Option<&str>, desired: &str, loaded: bool) -> Plan {
    match (existing == Some(desired), loaded) {
        (true, true) => Plan::KeepRunning,
        (_, loaded) => Plan::Bootstrap { replace: loaded },
    }
}

/// The result of a successful [`register`] call.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyRunning,
    Registered,
    Replaced,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Outcome::AlreadyRunning => "already registered",
            Outcome::Registered => "registered",
            Outcome::Replaced => "re-registered with a new plist",
        })
    }
}

#[derive(Debug)]
pub enum Error {
    ProgramMissing(PathBuf),
    NoHome,
    Io(io::Error),
    Launchctl { args: String, stderr: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ProgramMissing(path) => write!(
                f,
                "{} not found; build it with `cargo build -p formationd`",
                path.display()
            ),
            Error::NoHome => f.write_str("HOME is not set"),
            Error::Io(err) => write!(f, "{err}"),
            Error::Launchctl { args, stderr } => {
                write!(f, "`launchctl {args}` failed: {}", stderr.trim())
            }
        }
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Error::Io(err)
    }
}

/// Makes sure formationd runs as this user's LaunchAgent.
pub fn register(app_identifier: &str) -> Result<Outcome, Error> {
    let program = std::env::current_exe()?
        .canonicalize()?
        .with_file_name(PROGRAM_NAME);
    if !program.is_file() {
        return Err(Error::ProgramMissing(program));
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(Error::NoHome)?);
    let label = label(app_identifier);
    let plist_path = home.join(format!("Library/LaunchAgents/{label}.plist"));
    let log_dir = home.join("Library/Logs/Formation");
    let desired = render_plist(&label, &program, &log_dir.join("formationd.log"));

    // SAFETY: getuid has no preconditions and cannot fail.
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let service = format!("{domain}/{label}");
    let loaded = launchctl(&["print", &service])?.status.success();
    let existing = fs::read_to_string(&plist_path).ok();

    match plan(existing.as_deref(), &desired, loaded) {
        Plan::KeepRunning => Ok(Outcome::AlreadyRunning),
        Plan::Bootstrap { replace } => {
            fs::create_dir_all(&log_dir)?;
            if let Some(dir) = plist_path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&plist_path, &desired)?;
            if replace {
                checked(&["bootout", &service])?;
            }
            checked(&["bootstrap", &domain, &plist_path.to_string_lossy()])?;
            Ok(if replace {
                Outcome::Replaced
            } else {
                Outcome::Registered
            })
        }
    }
}

fn launchctl(args: &[&str]) -> io::Result<Output> {
    Command::new("/bin/launchctl").args(args).output()
}

fn checked(args: &[&str]) -> Result<(), Error> {
    let output = launchctl(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Launchctl {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_appends_the_program_name() {
        assert_eq!(label("com.example.app"), "com.example.app.formationd");
    }

    #[test]
    fn plist_keeps_formationd_alive_and_logs_to_one_file() {
        let plist = render_plist(
            "com.example.app.formationd",
            Path::new("/Applications/Formation.app/Contents/MacOS/formationd"),
            Path::new("/Users/me/Library/Logs/Formation/formationd.log"),
        );
        assert!(plist.contains("<key>Label</key>\n\t<string>com.example.app.formationd</string>"));
        assert!(plist.contains(
            "<string>/Applications/Formation.app/Contents/MacOS/formationd</string>"
        ));
        assert!(plist.contains("<key>KeepAlive</key>\n\t<true/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert_eq!(
            plist
                .matches("<string>/Users/me/Library/Logs/Formation/formationd.log</string>")
                .count(),
            2
        );
    }

    #[test]
    fn plist_escapes_paths() {
        let plist = render_plist(
            "l",
            Path::new("/Users/me/A&B <dev>/formationd"),
            Path::new("/tmp/log"),
        );
        assert!(plist.contains("<string>/Users/me/A&amp;B &lt;dev&gt;/formationd</string>"));
    }

    #[test]
    fn same_plist_already_loaded_keeps_formationd_running() {
        assert_eq!(plan(Some("x"), "x", true), Plan::KeepRunning);
    }

    #[test]
    fn changed_plist_replaces_the_loaded_service() {
        assert_eq!(
            plan(Some("old"), "new", true),
            Plan::Bootstrap { replace: true }
        );
    }

    #[test]
    fn missing_or_unloaded_service_is_bootstrapped() {
        assert_eq!(plan(None, "x", false), Plan::Bootstrap { replace: false });
        assert_eq!(
            plan(Some("x"), "x", false),
            Plan::Bootstrap { replace: false }
        );
        assert_eq!(plan(None, "x", true), Plan::Bootstrap { replace: true });
    }
}
