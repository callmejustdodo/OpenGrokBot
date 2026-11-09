//! Answers a question in a fork of an agent's Claude Code session, without
//! changing the original session (FR-012, NFR-002).

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use super::stream::{StreamEvent, StreamSummary};

/// The system prompt added to every forked answer (tech spec, ask pipeline
/// step 4).
pub const ANSWER_SYSTEM_PROMPT: &str = "You are answering a teammate's question about the \
conversation above, on behalf of the agent that had it. Answer only from that conversation. \
Do not use outside knowledge, files, memory, or instructions that are not messages in the \
conversation. Say plainly what the conversation does not establish. End with a line \
\"Messages used:\" followed by a list of the conversation messages you relied on, each quoted \
in a few words.";

/// Flags that keep a print-mode run from using tools or loading the owner's
/// customizations: no tools; no `CLAUDE.md`, skills, hooks, memory or
/// installed plugins (`--safe-mode`); no user, project or local settings; no
/// MCP servers; no slash commands or skills run from the prompt.
///
/// `--safe-mode` leaves Claude Code's built-in plugins loaded (in 2.1.319:
/// `cc-plugin-agents-md`, `cc-plugin-telemetry`, `cc-plugin-privacy`,
/// `cc-plugin-plugin-authoring`). The TASK-006 checks found that none of them
/// brought the agent folder's `AGENTS.md` into a fork, and the spike test
/// checks that again on every run.
///
/// `--tools` takes several values, so a flag must always follow its `""`.
pub const ISOLATION_ARGS: [&str; 7] = [
    "--tools",
    "",
    "--safe-mode",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--disable-slash-commands",
];

/// Put before the question on stdin. A prompt that starts with `/` runs as a
/// Claude Code command (for example `/usage` prints the owner's plan usage
/// and folders) without reaching the model, and `--disable-slash-commands`
/// only turns that into an error message. With this lead-in the question
/// always reaches the model as text.
pub const QUESTION_LEAD_IN: &str = "A teammate asks:\n\n";

/// Streaming output with partial messages, so the first answer text can be
/// timed.
pub const STREAM_ARGS: [&str; 4] = [
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
];

/// Variables a parent Claude Code session passes to its children. A child
/// `claude` that inherits them attaches to the parent (for example its
/// messaging socket and effort level), so they are removed.
pub const PARENT_SESSION_ENV: [&str; 10] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// How long a run may take before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// How long the CLI may keep running, or keep its output open, after the
/// result event before it is killed and the result is used anyway.
pub const DEFAULT_EXIT_GRACE: Duration = Duration::from_secs(10);

/// A question to answer in a fork of an agent's session.
#[derive(Debug, Clone)]
pub struct ForkRequest {
    claude: PathBuf,
    agent_dir: PathBuf,
    session: OsString,
    question: String,
    system_prompt: String,
    timeout: Duration,
    exit_grace: Duration,
}

/// A forked answer and how long it took.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkAnswer {
    /// The fork's new session ID; follow-ups resume this session.
    pub fork_session_id: String,
    /// The answer, or the CLI's error message when `is_error` is set.
    pub answer: String,
    pub is_error: bool,
    pub terminal_reason: Option<String>,
    /// From spawning the CLI to the first answer text.
    pub ttft: Option<Duration>,
    /// From spawning the CLI to the result event, which carries the full answer.
    pub total: Option<Duration>,
    /// From spawning the CLI to its exit, or to when it was killed after
    /// lingering past the result.
    pub exited: Duration,
    /// Model turns in the run; a local command would show 0.
    pub num_turns: Option<u64>,
    /// The tools the CLI listed at start: `Some(empty)` for every answer
    /// [`ForkRequest::run`] returns, or `None` if the CLI did not say.
    pub tools: Option<Vec<String>>,
    /// The CLI's own time to first text (`ttft_ms`).
    pub cli_ttft_ms: Option<u64>,
    /// The CLI's own run time (`duration_ms`).
    pub cli_duration_ms: Option<u64>,
    /// The cost of this answer alone.
    pub cost_usd: Option<f64>,
}

/// Why a forked answer could not be produced.
#[derive(Debug)]
pub enum AskError {
    /// The session must be a session ID or a transcript path, and must not
    /// look like a flag.
    InvalidSession(OsString),
    /// The CLI could not be started.
    Spawn(io::Error),
    /// Writing the question or waiting for the CLI failed.
    Io(io::Error),
    /// The CLI did not finish in time and was killed.
    Timeout(Duration),
    /// The CLI exited without a result event.
    NoResult { status: ExitStatus, stderr: String },
    /// The stream never named a session.
    NoSessionId,
    /// The CLI answered in the original session instead of a fork.
    NotForked(String),
    /// The CLI started with tools despite `--tools ""`, and was killed.
    ToolsEnabled(Vec<String>),
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSession(session) => write!(f, "invalid session to fork: {session:?}"),
            Self::Spawn(err) => write!(f, "could not start claude: {err}"),
            Self::Io(err) => write!(f, "claude I/O failed: {err}"),
            Self::Timeout(limit) => write!(f, "claude did not finish within {limit:?}"),
            Self::NoResult { status, stderr } => {
                write!(
                    f,
                    "claude exited ({status}) without a result: {}",
                    stderr.trim()
                )
            }
            Self::NoSessionId => f.write_str("claude output named no session"),
            Self::NotForked(id) => write!(f, "claude answered in the original session {id}"),
            Self::ToolsEnabled(tools) => {
                write!(f, "claude started with tools enabled: {}", tools.join(", "))
            }
        }
    }
}

impl std::error::Error for AskError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(err) | Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl ForkRequest {
    /// A question for the session `session` (a session ID, or a transcript
    /// path) of the agent working in `agent_dir`.
    pub fn new(
        agent_dir: impl Into<PathBuf>,
        session: impl Into<OsString>,
        question: impl Into<String>,
    ) -> Self {
        Self {
            claude: PathBuf::from("claude"),
            agent_dir: agent_dir.into(),
            session: session.into(),
            question: question.into(),
            system_prompt: ANSWER_SYSTEM_PROMPT.to_owned(),
            timeout: DEFAULT_TIMEOUT,
            exit_grace: DEFAULT_EXIT_GRACE,
        }
    }

    /// The `claude` executable to run (default: `claude` on `PATH`).
    pub fn claude_binary(mut self, path: impl Into<PathBuf>) -> Self {
        self.claude = path.into();
        self
    }

    /// Replaces [`ANSWER_SYSTEM_PROMPT`].
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Replaces [`DEFAULT_EXIT_GRACE`].
    pub fn exit_grace(mut self, grace: Duration) -> Self {
        self.exit_grace = grace;
        self
    }

    /// The text written to the CLI's stdin: [`QUESTION_LEAD_IN`] and the
    /// question.
    pub fn prompt(&self) -> String {
        format!("{QUESTION_LEAD_IN}{}", self.question)
    }

    /// The CLI arguments. The question is not among them: it goes on stdin.
    pub fn args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-p".into(),
            "--resume".into(),
            self.session.clone(),
            "--fork-session".into(),
        ];
        args.extend(ISOLATION_ARGS.map(OsString::from));
        // Without `--system-prompt-snapshot off` a resumed session reuses its
        // recorded system prompt and ignores the appended one.
        args.extend(
            ["--system-prompt-snapshot", "off", "--append-system-prompt"].map(OsString::from),
        );
        args.push(self.system_prompt.clone().into());
        args.extend(STREAM_ARGS.map(OsString::from));
        args
    }

    /// The command to spawn, in the agent folder.
    pub fn command(&self) -> Command {
        let mut command = claude_command(&self.claude, &self.agent_dir);
        command.args(self.args());
        command
    }

    /// Runs the fork and waits for the answer. The CLI is killed as soon as
    /// it reports any tool.
    pub fn run(&self) -> Result<ForkAnswer, AskError> {
        let session = self.session.as_os_str();
        if session.is_empty() || session.as_encoded_bytes().starts_with(b"-") {
            return Err(AskError::InvalidSession(self.session.clone()));
        }
        let limits = PrintLimits {
            timeout: self.timeout,
            exit_grace: self.exit_grace,
            forbid_tools: true,
        };
        let run = run_print(self.command(), &self.prompt(), limits)?;
        let PrintRun {
            summary,
            first_text,
            result_at,
            exited,
            status,
            stderr,
        } = run;
        let Some(result) = summary.result else {
            return Err(AskError::NoResult { status, stderr });
        };
        let fork_session_id = summary.session_id.ok_or(AskError::NoSessionId)?;
        if Path::new(session).file_stem() == Some(OsStr::new(&fork_session_id)) {
            return Err(AskError::NotForked(fork_session_id));
        }
        let answer = if result.text.is_empty() {
            summary.text
        } else {
            result.text
        };
        Ok(ForkAnswer {
            fork_session_id,
            answer,
            is_error: result.is_error,
            terminal_reason: result.terminal_reason,
            ttft: first_text,
            total: result_at,
            exited,
            num_turns: result.num_turns,
            tools: summary.tools,
            cli_ttft_ms: result.cli_ttft_ms,
            cli_duration_ms: result.cli_duration_ms,
            cost_usd: result.cost_usd,
        })
    }
}

/// A `claude` command in `cwd` with piped standard streams and without the
/// parent session's variables.
pub(crate) fn claude_command(claude: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(claude);
    command
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in PARENT_SESSION_ENV {
        command.env_remove(var);
    }
    command
}

/// Limits for one `claude -p` run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PrintLimits {
    /// The run is killed after this long; without a result it fails.
    pub timeout: Duration,
    /// After the result event, how long the CLI may take to exit and close
    /// its output before it is killed and the result is used.
    pub exit_grace: Duration,
    /// Kill the run as soon as its `init` event lists any tool.
    pub forbid_tools: bool,
}

/// One finished `claude -p` stream-json run. Times count from the spawn.
#[derive(Debug)]
pub(crate) struct PrintRun {
    pub summary: StreamSummary,
    pub first_text: Option<Duration>,
    pub result_at: Option<Duration>,
    pub exited: Duration,
    pub status: ExitStatus,
    pub stderr: String,
}

/// How long to wait for the stderr reader once stdout has closed.
const STDERR_DRAIN: Duration = Duration::from_secs(2);

/// Spawns `command`, writes `prompt` to its stdin, and reads its stream-json
/// output line by line, timing the first answer text and the result.
///
/// Every wait is bounded: a CLI that lingers after its result, or a
/// background process that keeps the output pipes open, costs at most
/// `exit_grace` and never the answer.
pub(crate) fn run_print(
    mut command: Command,
    prompt: &str,
    limits: PrintLimits,
) -> Result<PrintRun, AskError> {
    let start = Instant::now();
    let mut child = command.spawn().map_err(AskError::Spawn)?;

    // Written on its own thread so a long prompt cannot block reading the
    // output; closing stdin when the thread ends tells the CLI to start.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let prompt = prompt.to_owned();
    let (writer_tx, writer_rx) = mpsc::channel();
    thread::spawn(move || {
        let written = match stdin.write_all(prompt.as_bytes()) {
            // A CLI that exits before reading its input explains why on stderr.
            Err(err) if err.kind() != io::ErrorKind::BrokenPipe => Err(err),
            _ => Ok(()),
        };
        let _ = writer_tx.send(written);
    });

    let mut stderr = child.stderr.take().expect("stderr is piped");
    let (stderr_tx, stderr_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        let _ = stderr_tx.send(text);
    });
    let stdout = child.stdout.take().expect("stdout is piped");
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines_tx.send((Instant::now(), line)).is_err() {
                break;
            }
        }
    });

    let deadline = start + limits.timeout;
    let mut wait_until = deadline;
    let mut stdout_closed = false;
    let mut summary = StreamSummary::default();
    let (mut first_text, mut result_at) = (None, None);
    loop {
        match lines_rx.recv_timeout(wait_until.saturating_duration_since(Instant::now())) {
            Ok((at, line)) => match summary.feed(&line) {
                StreamEvent::Init {
                    tools: Some(tools), ..
                } if limits.forbid_tools && !tools.is_empty() => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AskError::ToolsEnabled(tools));
                }
                StreamEvent::TextDelta(text) if first_text.is_none() && !text.is_empty() => {
                    first_text = Some(at.duration_since(start));
                }
                StreamEvent::Result(_) => {
                    result_at = Some(at.duration_since(start));
                    wait_until = deadline.min(at + limits.exit_grace);
                }
                _ => {}
            },
            Err(RecvTimeoutError::Disconnected) => {
                stdout_closed = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => break,
        }
    }

    // Stdout is closed or the wait ran out; give the CLI until the same
    // moment to exit.
    let status = loop {
        if let Some(status) = child.try_wait().map_err(AskError::Io)? {
            break status;
        }
        if Instant::now() >= wait_until {
            let _ = child.kill();
            let status = child.wait().map_err(AskError::Io)?;
            if summary.result.is_none() {
                return Err(AskError::Timeout(limits.timeout));
            }
            break status;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let exited = start.elapsed();

    // A background process that inherited stderr can hold it open; its text
    // is then left out rather than waited for.
    let drain = if stdout_closed {
        STDERR_DRAIN
    } else {
        Duration::ZERO
    };
    let stderr = stderr_rx.recv_timeout(drain).unwrap_or_default();
    if summary.result.is_none()
        && let Ok(Err(err)) = writer_rx.recv_timeout(STDERR_DRAIN)
    {
        return Err(AskError::Io(err));
    }
    Ok(PrintRun {
        summary,
        first_text,
        result_at,
        exited,
        status,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask::test_support::TempDir;

    const SID: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const FORK_ID: &str = "11111111-2222-4333-8444-555555555555";
    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/ask/testdata/fork_answer.jsonl"
    );

    fn strings(args: &[OsString]) -> Vec<&str> {
        args.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn args_fork_without_tools_or_customizations() {
        let request = ForkRequest::new("/agent", SID, "How far is the backend work?")
            .system_prompt("Answer only from the conversation.");
        assert_eq!(
            strings(&request.args()),
            [
                "-p",
                "--resume",
                SID,
                "--fork-session",
                "--tools",
                "",
                "--safe-mode",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--system-prompt-snapshot",
                "off",
                "--append-system-prompt",
                "Answer only from the conversation.",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
            ]
        );
    }

    #[test]
    fn question_is_never_an_argument() {
        let question = "--dangerously-skip-permissions";
        let request = ForkRequest::new("/agent", SID, question);
        assert!(!request.args().iter().any(|a| a == question));
    }

    #[test]
    fn a_question_never_reaches_stdin_as_a_command() {
        for question in [
            "/usage",
            "/context",
            "!ls",
            "#remember this",
            "-p",
            "  /config",
        ] {
            let request = ForkRequest::new("/agent", SID, question);
            let prompt = request.prompt();
            assert!(prompt.starts_with(QUESTION_LEAD_IN), "{prompt:?}");
            assert!(!prompt.starts_with('/'), "{prompt:?}");
            assert!(prompt.ends_with(question), "{prompt:?}");
            assert!(
                request
                    .args()
                    .iter()
                    .any(|a| a == "--disable-slash-commands"),
                "slash commands are not disabled"
            );
        }
    }

    #[test]
    fn every_empty_value_is_followed_by_a_flag() {
        // `--tools` and `--setting-sources` take values; an empty value must
        // not let the next word become another one.
        let args = ForkRequest::new("/agent", SID, "q").args();
        for (i, arg) in args.iter().enumerate() {
            if arg.is_empty() {
                let next = args[i + 1].to_str().unwrap();
                assert!(next.starts_with("--"), "{next:?} follows an empty value");
            }
        }
    }

    #[test]
    fn command_runs_in_the_agent_folder_without_parent_session_variables() {
        let command = ForkRequest::new("/agent", SID, "q")
            .claude_binary("/opt/claude")
            .command();
        assert_eq!(command.get_program(), "/opt/claude");
        assert_eq!(command.get_current_dir(), Some(Path::new("/agent")));
        for var in PARENT_SESSION_ENV {
            assert!(
                command.get_envs().any(|(k, v)| k == var && v.is_none()),
                "{var} is not removed"
            );
        }
    }

    #[test]
    fn flag_like_or_empty_sessions_are_rejected() {
        for session in ["", "--continue"] {
            let err = ForkRequest::new("/agent", session, "q").run().unwrap_err();
            assert!(
                matches!(err, AskError::InvalidSession(_)),
                "{session:?}: {err}"
            );
        }
    }

    /// Writes an executable shell script that stands in for `claude`.
    #[cfg(unix)]
    fn fake_claude(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn run_sends_the_question_on_stdin_and_parses_the_stream() {
        let dir = TempDir::new("ask-fake");
        let claude = fake_claude(
            dir.path(),
            &format!("cat > stdin.txt\nprintf '%s\\n' \"$@\" > args.txt\ncat '{FIXTURE}'"),
        );
        let question = "How far is the backend work?";
        let request = ForkRequest::new(dir.path(), SID, question).claude_binary(&claude);
        let answer = request.run().unwrap();

        assert_eq!(answer.fork_session_id, FORK_ID);
        assert!(!answer.is_error);
        assert!(answer.answer.starts_with("The login API cleanup is done"));
        assert_eq!(answer.cli_ttft_ms, Some(5028));
        assert_eq!(answer.cost_usd, Some(0.0679436));
        assert_eq!(answer.num_turns, Some(1));
        assert_eq!(answer.tools, Some(vec![]));
        let (ttft, total) = (answer.ttft.unwrap(), answer.total.unwrap());
        assert!(ttft <= total && total <= answer.exited);

        // The script ran in the agent folder and got the question on stdin.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap(),
            request.prompt()
        );
        let args = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
        let expected: Vec<String> = ForkRequest::new(dir.path(), SID, question)
            .args()
            .iter()
            .map(|a| a.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(args.lines().collect::<Vec<_>>(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn run_reports_a_cli_that_exits_without_a_result() {
        let dir = TempDir::new("ask-fake");
        let claude = fake_claude(dir.path(), "echo 'unknown option' >&2\nexit 1");
        let err = ForkRequest::new(dir.path(), SID, "q")
            .claude_binary(&claude)
            .run()
            .unwrap_err();
        let AskError::NoResult { status, stderr } = err else {
            panic!("expected NoResult, got {err}");
        };
        assert!(!status.success());
        assert!(stderr.contains("unknown option"));
    }

    #[cfg(unix)]
    #[test]
    fn run_refuses_an_answer_written_to_the_original_session() {
        let dir = TempDir::new("ask-fake");
        // The fixture's session is FORK_ID; resuming FORK_ID itself means the
        // CLI did not fork.
        let claude = fake_claude(dir.path(), &format!("cat >/dev/null\ncat '{FIXTURE}'"));
        let err = ForkRequest::new(dir.path(), FORK_ID, "q")
            .claude_binary(&claude)
            .run()
            .unwrap_err();
        assert!(
            matches!(err, AskError::NotForked(ref id) if id == FORK_ID),
            "{err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_kills_a_cli_that_takes_too_long() {
        let dir = TempDir::new("ask-fake");
        let claude = fake_claude(dir.path(), "exec sleep 30");
        let started = Instant::now();
        let err = ForkRequest::new(dir.path(), SID, "q")
            .claude_binary(&claude)
            .timeout(Duration::from_millis(300))
            .run()
            .unwrap_err();
        assert!(matches!(err, AskError::Timeout(_)), "{err}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn run_keeps_the_answer_when_the_cli_lingers_after_the_result() {
        let dir = TempDir::new("ask-fake");
        let claude = fake_claude(
            dir.path(),
            &format!("cat >/dev/null\ncat '{FIXTURE}'\nexec sleep 30"),
        );
        let started = Instant::now();
        let answer = ForkRequest::new(dir.path(), SID, "q")
            .claude_binary(&claude)
            .timeout(Duration::from_secs(20))
            .exit_grace(Duration::from_millis(300))
            .run()
            .unwrap();
        assert_eq!(answer.fork_session_id, FORK_ID);
        assert!(answer.answer.starts_with("The login API cleanup is done"));
        assert!(answer.total.unwrap() <= answer.exited);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn run_keeps_the_answer_when_a_background_process_holds_the_output() {
        let dir = TempDir::new("ask-fake");
        // The background `sleep` inherits stdout and stderr and keeps them
        // open after the script exits.
        let claude = fake_claude(
            dir.path(),
            &format!("cat >/dev/null\ncat '{FIXTURE}'\nsleep 5 &\nexit 0"),
        );
        let started = Instant::now();
        let answer = ForkRequest::new(dir.path(), SID, "q")
            .claude_binary(&claude)
            .timeout(Duration::from_secs(20))
            .exit_grace(Duration::from_millis(300))
            .run()
            .unwrap();
        assert_eq!(answer.fork_session_id, FORK_ID);
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[cfg(unix)]
    #[test]
    fn run_kills_a_cli_that_starts_with_tools() {
        let dir = TempDir::new("ask-fake");
        let init = r#"{"type":"system","subtype":"init","session_id":"s","tools":["Read","Bash"],"mcp_servers":[]}"#;
        let claude = fake_claude(
            dir.path(),
            &format!("cat >/dev/null\necho '{init}'\nexec sleep 30"),
        );
        let started = Instant::now();
        let err = ForkRequest::new(dir.path(), SID, "q")
            .claude_binary(&claude)
            .run()
            .unwrap_err();
        assert!(
            matches!(err, AskError::ToolsEnabled(ref tools) if tools == &["Read", "Bash"]),
            "{err}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
