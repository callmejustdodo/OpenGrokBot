//! Ask pipeline: answering a teammate's question from an agent's session
//! (FR-010 to FR-013, tech spec "Ask pipeline").
//!
//! TASK-019 builds the pipeline on the owner's Mac:
//!
//! - [`pipeline`] takes a received question to an answer, a routing choice, or
//!   a failure; continues threads with follow-ups; refreshes. Answers come from
//!   `Harness::run_fork` with no tools, never from the main session.
//! - [`routing`] decides confident or unsure from a [`routing::Scorer`];
//!   [`scorer`] is the real one, a short isolated Claude Code call.
//! - [`answer`] post-processes a forked answer: excerpts, `as_of`,
//!   `possibly_stale`, visibility, and masking ([`mask`]).
//! - [`endpoints`] serves teammates' `POST /v1/ask` and question actions with
//!   the pipeline ([`OwnerAsks`]), answering in the background and publishing
//!   every change to the asker.
//!
//! Sharing and visibility settings live in [`crate::policy::sharing`].
//!
//! The TASK-006 spike answered a question in a fork of the agent's Claude Code
//! session and showed that the original session's transcript is byte-identical
//! afterwards (FR-012, NFR-002):
//!
//! - [`fork`] builds and runs the answer command and measures it.
//! - [`stream`] parses the CLI's `stream-json` output.
//! - [`transcript`] finds a session's transcript file.
//!
//! What the TASK-006 experiment (Claude Code 2.1.319) established, and why the
//! command looks the way it does:
//!
//! - `--resume <id> --fork-session` writes the answer to a new session and
//!   writes nothing to the original transcript. While the agent is idle the
//!   original stays byte-identical, modification time included; the spike
//!   test checks this for a text-only session, for a session that used a
//!   tool, and for a copy cut right after an unanswered tool call. While the
//!   agent is mid-turn, only the agent's own process appends to the file:
//!   the earlier bytes stay an exact prefix and no fork content (question
//!   text or fork session ID) appears. Checks against a live agent should
//!   therefore compare by prefix and scan for fork content, not compare the
//!   whole file.
//! - A resumed session reuses the system prompt recorded in its transcript, so
//!   `--append-system-prompt` is silently ignored unless
//!   `--system-prompt-snapshot off` is passed.
//! - With the owner's settings loaded, a fork picks up the agent folder's
//!   current `CLAUDE.md`, the owner's hooks (which run on every fork) and MCP
//!   servers. `--safe-mode --setting-sources "" --strict-mcp-config` turns
//!   those off, along with skills, memory and installed plugins, and keeps
//!   subscription sign-in working; `--bare` does not, because it never reads
//!   OAuth credentials. Claude Code's built-in plugins (such as
//!   `cc-plugin-agents-md`) still load; none of them brought `AGENTS.md` into
//!   a fork, and the spike test plants canaries in `AGENTS.md` and `CLAUDE.md`
//!   to keep checking that.
//! - The question goes to the CLI on stdin, never as an argument: `--tools`
//!   takes several values and would swallow a trailing prompt, and a prompt
//!   that starts with `-` or names a subcommand would be parsed as one.
//! - A prompt that starts with `/` runs as a Claude Code command without the
//!   model or the ask system prompt: `/usage` answered with the owner's plan
//!   usage, time zone and project folders. `--disable-slash-commands` only
//!   turns that into "isn't available", so the question also goes after a
//!   fixed lead-in ([`fork::QUESTION_LEAD_IN`]) and always reaches the model.
//! - The `init` event lists the run's tools. [`ForkRequest::run`] kills the
//!   CLI if that list is not empty, in case a CLI version ignores
//!   `--tools ""`.
//! - Waits are bounded: a CLI that lingers after its result, or a background
//!   process that keeps its output open, is killed after a short grace period
//!   and the answer is kept.
//!
//! Measured with Claude Code 2.1.319, model `claude-opus-5-5` and the
//! isolation flags, from spawning the CLI. Runs 1-4 used the spike before
//! `--disable-slash-commands` and the question lead-in were added; run 5 is
//! the current spike test.
//!
//! | Run | Original session, question | First answer text | Full answer | CLI exit |
//! | --- | --- | --- | --- | --- |
//! | 1 | one text turn, progress question | 2.9 s | 4.5 s | 5.3 s |
//! | 2 | one text turn, progress question | 5.4 s | 6.9 s | 7.5 s |
//! | 3 | one text turn, progress question | 2.9 s | 4.7 s | 5.5 s |
//! | 4 | one text turn, progress question | 2.4 s | 3.6 s | 4.5 s |
//! | 5 | one text turn, progress question | 6.5 s | 7.8 s | 8.4 s |
//! | 5 | one text turn, `/usage` | 3.8 s | 5.1 s | 5.9 s |
//! | 5 | one turn with a `Read` call, status question | 2.4 s | 3.5 s | 4.3 s |
//! | 5 | cut after the `Read` call, status question | 2.6 s | 4.1 s | 4.9 s |
//!
//! Each answer cost about $0.011-0.013 (`model_usage_since_last_result`).
//! Resuming the cut copy, the CLI fills in the missing tool result in the
//! fork only ("Tool call interrupted ...", then "No response requested."),
//! and answers may list those CLI-made messages as used.
//!
//! Known limits the spike found, and how TASK-019 handles them:
//!
//! - Every fork leaves a full copy of the conversation as
//!   `<fork-session-id>.jsonl` beside the original. The pipeline records each
//!   thread's newest fork, deletes the fork it replaces, and deletes the last
//!   one with the thread ([`pipeline::AskPipeline::discard_thread`]). Session
//!   detection must still skip the recorded forks (FR-006).
//! - No flag removes content that is already part of the original
//!   conversation (for example `SessionStart` hook output or instructions the
//!   agent loaded), and the CLI attaches the account email and environment to
//!   each new turn. Answers and excerpts are masked ([`mask`]), including the
//!   owner's email addresses when the pipeline is given them; excerpts show only
//!   user and assistant messages, never tool output.
//! - A fork taken while the agent is mid-turn sees the pending user message
//!   but none of the reply in progress, and reports that request as still
//!   open. The question then carries a note saying so
//!   ([`pipeline::MID_TURN_NOTE`]) instead of the fork being cut.
//! - The process runner in [`fork`] (`claude_command`, `run_print`), the
//!   isolation flags and the list of parent-session variables are spike code
//!   and inputs to TASK-022, where role A builds the forked, tool-less run
//!   behind the Harness port. Answers go through that port; only the routing
//!   call ([`scorer`]), which resumes no session, uses this runner.

pub mod answer;
pub mod endpoints;
pub(crate) mod excerpts;
pub mod fork;
pub mod mask;
pub mod pipeline;
pub mod routing;
pub mod scorer;
pub mod stream;
pub mod transcript;

pub use endpoints::OwnerAsks;
pub use fork::{ANSWER_SYSTEM_PROMPT, AskError, ForkAnswer, ForkRequest, QUESTION_LEAD_IN};
pub use stream::{StreamEvent, StreamResult, StreamSummary};
pub use transcript::{
    claude_config_dir, find_transcript, project_dir, project_dir_name, transcript_path,
};

#[cfg(test)]
mod spike_test;

#[cfg(test)]
pub(crate) mod test_support {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A directory removed with everything in it when dropped.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            Self::new_in(&std::env::temp_dir(), label)
        }

        pub fn new_in(base: &Path, label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("formation-{label}-{}-{millis}-{n}", process::id()));
            fs::create_dir_all(&path).expect("create a temporary directory");
            Self(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
