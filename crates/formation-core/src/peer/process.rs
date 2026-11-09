//! Runs helper programs (`lsof`, `tailscale`, `gh`) by absolute path with a
//! deadline. formationd runs under launchd, whose `PATH` is only
//! `/usr/bin:/bin:/usr/sbin:/sbin`, so nothing here searches `PATH`.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// How long the output pipes may stay open after the deadline or after the
/// program ends, before [`run`] stops waiting for them.
const PIPE_GRACE: Duration = Duration::from_millis(250);

const SIGKILL: i32 = 9;

// kill(2) takes two integers and touches no memory.
unsafe extern "C" {
    safe fn kill(pid: i32, sig: i32) -> i32;
}

#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// The first non-empty line of stderr, else of stdout, for error messages.
    pub fn first_line(&self) -> String {
        [&self.stderr, &self.stdout]
            .into_iter()
            .flat_map(|bytes| {
                String::from_utf8_lossy(bytes)
                    .lines()
                    .map(|line| line.trim().to_owned())
                    .filter(|line| !line.is_empty())
                    .take(1)
                    .collect::<Vec<_>>()
            })
            .next()
            .unwrap_or_default()
    }
}

/// An environment change for the child: `Some` sets the variable, `None`
/// removes it. Everything else is inherited.
pub type EnvChange<'a> = (&'a str, Option<&'a str>);

/// Runs `program` with `args`, no stdin, and kills it after `timeout`.
///
/// The program runs in its own process group, and the whole group is killed at
/// the deadline, so a descendant that inherited the output pipes cannot keep
/// them open. Whatever happens, `run` returns within `timeout` plus
/// [`PIPE_GRACE`]: if the program exits but something keeps its output open,
/// that is a timeout too.
pub fn run(
    program: &Path,
    args: &[&str],
    env: &[EnvChange<'_>],
    timeout: Duration,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, value) in env {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = wait_until(&mut child, deadline);
    let collect_by = deadline.max(Instant::now()) + PIPE_GRACE;
    let stdout = collect(&stdout, collect_by);
    let stderr = collect(&stderr, collect_by);
    let context =
        |error: io::Error| io::Error::new(error.kind(), format!("{}: {error}", program.display()));
    let status = status.map_err(context)?;
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Err(context(io::Error::new(
            io::ErrorKind::TimedOut,
            "exited, but its output stayed open past the deadline",
        )));
    };
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Reads `pipe` to the end on its own thread and sends the bytes when done.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = sender.send(bytes);
    });
    receiver
}

/// The drained output, or `None` if the pipe is still open at `by`. The drain
/// thread then ends on its own once the last writer exits.
fn collect(output: &Receiver<Vec<u8>>, by: Instant) -> Option<Vec<u8>> {
    output
        .recv_timeout(by.saturating_duration_since(Instant::now()))
        .ok()
}

fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            // The child is not reaped yet, so its PID still names its process
            // group (`process_group(0)`) and no other.
            if let Ok(pid) = i32::try_from(child.id()) {
                kill(-pid, SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// The first candidate that is an executable file.
pub fn first_executable<'a>(candidates: impl IntoIterator<Item = &'a str>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .map(PathBuf::from)
        .find(|path| is_executable(path))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_applies_env_changes() {
        let output = run(
            Path::new("/bin/sh"),
            &["-c", "printf \"$A-${B:-unset}\"; echo oops >&2; exit 3"],
            &[("A", Some("set")), ("B", None)],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.stdout, b"set-unset");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.first_line(), "oops");
    }

    #[test]
    fn kills_a_program_that_overruns() {
        let started = Instant::now();
        let error = run(
            Path::new("/bin/sleep"),
            &["5"],
            &[],
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn kills_descendants_that_hold_the_output_open() {
        // The background `sleep` inherits stdout; killing only the shell would
        // leave it holding the pipe for 20 s.
        let pid_file = std::env::temp_dir().join(format!(
            "formation-peer-{}-background-pid",
            std::process::id()
        ));
        let script = format!("sleep 20 & echo $! > '{}'; sleep 20", pid_file.display());
        let started = Instant::now();
        let error = run(
            Path::new("/bin/sh"),
            &["-c", &script],
            &[],
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );

        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let _ = std::fs::remove_file(&pid_file);
        // Once killed it is reparented and reaped; kill(pid, 0) then fails.
        let deadline = Instant::now() + Duration::from_secs(2);
        while kill(pid, 0) == 0 {
            assert!(
                Instant::now() < deadline,
                "the background sleep {pid} outlived run()"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn returns_at_the_deadline_when_the_output_outlives_the_program() {
        // The shell exits at once; its background child keeps stdout open.
        let started = Instant::now();
        let error = run(
            Path::new("/bin/sh"),
            &["-c", "sleep 2 &"],
            &[],
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("output stayed open"), "{error}");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn finds_only_executable_files() {
        assert_eq!(
            first_executable(["/nonexistent/tailscale", "/etc/hosts", "/bin", "/bin/sh"]),
            Some(PathBuf::from("/bin/sh"))
        );
        assert_eq!(first_executable(["/nonexistent/gh"]), None);
    }
}
