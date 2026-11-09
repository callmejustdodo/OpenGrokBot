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
use std::thread;
use std::time::Duration;

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
            replace_and_bootstrap(
                launchctl,
                thread::sleep,
                replace,
                &domain,
                &service,
                &plist_path.to_string_lossy(),
            )?;
            Ok(if replace {
                Outcome::Replaced
            } else {
                Outcome::Registered
            })
        }
    }
}

/// Restarts the loaded formationd, so it runs the binary now in the bundle
/// (after an update). The plist is unchanged, so [`register`] alone would keep
/// the old process running. This stops agent turns in progress.
pub fn restart(app_identifier: &str) -> Result<(), Error> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let service = format!(
        "gui/{}/{}",
        unsafe { libc::getuid() },
        label(app_identifier)
    );
    checked(&mut launchctl, &["kickstart", "-k", &service])
}

/// How often, and how many times, to check that a booted-out service is gone
/// (about 5 s in all).
const GONE_POLL_INTERVAL: Duration = Duration::from_millis(100);
const GONE_POLLS: u32 = 50;
/// How many times to try `bootstrap` while launchd reports error 5, and the
/// pause between tries.
const BOOTSTRAP_ATTEMPTS: u32 = 5;
const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Boots out the loaded service when `replace` is set, waits until launchd has
/// removed it, then bootstraps `plist`. `launchctl bootout` returns before the
/// service is gone, and a bootstrap in that window fails with
/// "Bootstrap failed: 5: Input/output error" (issue #181), so the bootstrap is
/// also retried a few times on that error.
fn replace_and_bootstrap(
    mut run: impl FnMut(&[&str]) -> io::Result<Output>,
    mut sleep: impl FnMut(Duration),
    replace: bool,
    domain: &str,
    service: &str,
    plist: &str,
) -> Result<(), Error> {
    if replace {
        checked(&mut run, &["bootout", service])?;
        for _ in 0..GONE_POLLS {
            if !run(&["print", service])?.status.success() {
                break;
            }
            sleep(GONE_POLL_INTERVAL);
        }
    }
    let mut attempt = 1;
    loop {
        match checked(&mut run, &["bootstrap", domain, plist]) {
            Err(Error::Launchctl { stderr, .. })
                if attempt < BOOTSTRAP_ATTEMPTS && stderr.contains("Input/output error") =>
            {
                attempt += 1;
                sleep(BOOTSTRAP_RETRY_DELAY);
            }
            result => return result,
        }
    }
}

fn launchctl(args: &[&str]) -> io::Result<Output> {
    Command::new("/bin/launchctl").args(args).output()
}

fn checked(
    run: &mut impl FnMut(&[&str]) -> io::Result<Output>,
    args: &[&str],
) -> Result<(), Error> {
    let output = run(args)?;
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
        assert!(
            plist
                .contains("<string>/Applications/Formation.app/Contents/MacOS/formationd</string>")
        );
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

    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    const IO_ERROR: &str = "Bootstrap failed: 5: Input/output error";

    /// A fake launchctl: records every call, keeps the service loaded for
    /// `prints_loaded` `print` calls after the bootout, and answers each
    /// `bootstrap` with the next entry of `bootstraps` (an error's stderr, or
    /// `None` for success).
    struct FakeLaunchctl {
        calls: Vec<String>,
        prints_loaded: u32,
        bootstraps: Vec<Option<&'static str>>,
    }

    impl FakeLaunchctl {
        fn run(&mut self, args: &[&str]) -> io::Result<Output> {
            self.calls.push(args.join(" "));
            let failure = match args[0] {
                "bootout" => None,
                "print" if self.prints_loaded > 0 => {
                    self.prints_loaded -= 1;
                    None
                }
                "print" => Some("Could not find service"),
                "bootstrap" => self.bootstraps.remove(0),
                other => panic!("unexpected launchctl {other}"),
            };
            Ok(Output {
                status: ExitStatus::from_raw(if failure.is_some() { 5 << 8 } else { 0 }),
                stdout: Vec::new(),
                stderr: failure.unwrap_or_default().as_bytes().to_vec(),
            })
        }
    }

    fn replace_with(fake: &mut FakeLaunchctl, sleeps: &mut Vec<Duration>) -> Result<(), Error> {
        replace_and_bootstrap(
            |args: &[&str]| fake.run(args),
            |delay| sleeps.push(delay),
            true,
            "gui/501",
            "gui/501/l",
            "/p.plist",
        )
    }

    #[test]
    fn replace_waits_for_the_old_service_to_go_before_bootstrapping() {
        let mut fake = FakeLaunchctl {
            calls: Vec::new(),
            prints_loaded: 3,
            bootstraps: vec![None],
        };
        let mut sleeps = Vec::new();
        replace_with(&mut fake, &mut sleeps).unwrap();
        assert_eq!(
            fake.calls,
            [
                "bootout gui/501/l",
                "print gui/501/l",
                "print gui/501/l",
                "print gui/501/l",
                "print gui/501/l",
                "bootstrap gui/501 /p.plist",
            ]
        );
        assert_eq!(sleeps, [GONE_POLL_INTERVAL; 3]);
    }

    #[test]
    fn bootstrap_is_retried_after_an_input_output_error() {
        let mut fake = FakeLaunchctl {
            calls: Vec::new(),
            prints_loaded: 0,
            bootstraps: vec![Some(IO_ERROR), None],
        };
        let mut sleeps = Vec::new();
        replace_with(&mut fake, &mut sleeps).unwrap();
        assert_eq!(
            fake.calls,
            [
                "bootout gui/501/l",
                "print gui/501/l",
                "bootstrap gui/501 /p.plist",
                "bootstrap gui/501 /p.plist",
            ]
        );
        assert_eq!(sleeps, [BOOTSTRAP_RETRY_DELAY]);
    }

    #[test]
    fn bootstrap_that_keeps_failing_returns_the_error() {
        let mut fake = FakeLaunchctl {
            calls: Vec::new(),
            prints_loaded: 0,
            bootstraps: vec![Some(IO_ERROR); BOOTSTRAP_ATTEMPTS as usize],
        };
        let mut sleeps = Vec::new();
        let err = replace_with(&mut fake, &mut sleeps).unwrap_err();
        assert!(
            matches!(&err, Error::Launchctl { args, stderr }
                if args == "bootstrap gui/501 /p.plist" && stderr == IO_ERROR),
            "{err:?}"
        );
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| c.starts_with("bootstrap"))
                .count(),
            BOOTSTRAP_ATTEMPTS as usize
        );
        assert_eq!(sleeps.len(), BOOTSTRAP_ATTEMPTS as usize - 1);
    }
}
