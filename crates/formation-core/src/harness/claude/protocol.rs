//! The line protocol between formationd and `claude -p` in stream-json mode.
//!
//! The types follow what Claude Code 2.1.319 wrote in `testkit/fixtures/claude/`. A field is
//! required when every recorded line has it. A line that breaks these types, or has a type they
//! do not name, decodes to [`Event::Unknown`] so a newer Claude Code cannot stop the reader.

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

/// One line of `claude` stdout.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    System(SystemEvent),
    StreamEvent(StreamEvent),
    Assistant(AssistantEvent),
    User(UserEvent),
    Result(ResultEvent),
    ControlResponse(ControlResponseEvent),
    RateLimitEvent(RateLimitEvent),
    /// The whole line, for any type or shape not modelled above.
    #[serde(skip)]
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SystemEvent {
    pub session_id: String,
    #[serde(flatten)]
    pub kind: SystemKind,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum SystemKind {
    Init {
        model: String,
        #[serde(rename = "permissionMode")]
        permission_mode: String,
        cwd: String,
    },
    Status {
        status: String,
    },
    HookStarted {
        hook_id: String,
        hook_name: String,
        hook_event: String,
    },
    HookResponse {
        hook_id: String,
        hook_name: String,
        hook_event: String,
        outcome: String,
        exit_code: Option<i64>,
    },
    TaskStarted {
        task_id: String,
        tool_use_id: Option<String>,
        description: String,
    },
    TaskNotification {
        task_id: String,
        tool_use_id: Option<String>,
        status: String,
        summary: String,
    },
    ThinkingTokens {
        estimated_tokens: u64,
    },
}

/// A partial-message event from `--include-partial-messages`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEvent {
    pub session_id: String,
    pub parent_tool_use_id: Option<String>,
    pub event: ApiEvent,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiEvent {
    MessageStart {
        message: StartedMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: ContentDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaBody,
    },
    MessageStop,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StartedMessage {
    pub id: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MessageDeltaBody {
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    /// A fragment of a tool call's JSON input; not valid JSON on its own.
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// A string in the fixtures; the Messages API also allows a list of blocks.
        content: Value,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantEvent {
    pub session_id: String,
    pub parent_tool_use_id: Option<String>,
    pub message: AssistantMessage,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantMessage {
    pub id: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserEvent {
    pub session_id: String,
    pub parent_tool_use_id: Option<String>,
    /// True when this echoes a message written to stdin, from `--replay-user-messages`.
    #[serde(rename = "isReplay", default)]
    pub is_replay: bool,
    pub message: UserMessage,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserMessage {
    pub content: UserContent,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// The end of a turn. An interrupted turn has subtype `error_during_execution` and no `result`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResultEvent {
    pub session_id: String,
    pub subtype: String,
    pub is_error: bool,
    pub result: Option<String>,
    pub terminal_reason: Option<String>,
    /// The HTTP status of a failed API call, or null. Kept raw because only `null`
    /// is recorded in the fixtures.
    pub api_error_status: Option<Value>,
    pub total_cost_usd: Option<f64>,
    pub usage: Option<Value>,
}

/// The answer to a stdin `control_request`, nested as Claude Code writes it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlResponseEvent {
    pub response: ControlResponse,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlResponse {
    pub subtype: String,
    pub request_id: String,
    pub response: Option<ControlResponseData>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlResponseData {
    /// Only ever an empty list in the fixtures, so the items stay raw.
    pub still_queued: Option<Vec<Value>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RateLimitEvent {
    pub session_id: String,
    pub rate_limit_info: Value,
}

/// Decodes one stdout line. Only invalid JSON is an error.
pub fn decode_stdout_line(line: &str) -> Result<Event, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    Ok(Event::deserialize(&value).unwrap_or(Event::Unknown(value)))
}

/// When a user message written during a running turn takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Now,
    Next,
    Later,
}

impl Priority {
    fn as_str(self) -> &'static str {
        match self {
            Priority::Now => "now",
            Priority::Next => "next",
            Priority::Later => "later",
        }
    }
}

/// One stdin line carrying a user message, without a trailing newline: the caller writes it.
///
/// With no priority, a message written during a turn joins that turn.
pub fn encode_user_message(text: &str, priority: Option<Priority>) -> String {
    let mut line = json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
    });
    if let Some(priority) = priority {
        line["priority"] = json!(priority.as_str());
    }
    line.to_string()
}

/// One stdin line interrupting the running turn, without a trailing newline: the caller writes it.
///
/// Claude Code answers with a `control_response` carrying `request_id`.
pub fn encode_interrupt(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
    .to_string()
}

/// How the process attaches to its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch<'a> {
    New { session_id: &'a str },
    Resume { session_id: &'a str },
}

/// Flags added to the launch arguments when set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaunchOptions<'a> {
    pub permission_prompt_tool: Option<&'a str>,
    pub mcp_config: Option<&'a str>,
    pub settings: Option<&'a str>,
    pub permission_mode: Option<&'a str>,
    /// The agent's repositories, each passed with `--add-dir` so Claude Code's
    /// tools can work there as well as in the agent folder (FR-001, FR-015).
    pub add_dirs: &'a [PathBuf],
}

/// The arguments for `claude`, not including the program name.
pub fn launch_args(launch: Launch<'_>, options: LaunchOptions<'_>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
    ]
    .map(String::from)
    .to_vec();
    let (flag, session_id) = match launch {
        Launch::New { session_id } => ("--session-id", session_id),
        Launch::Resume { session_id } => ("--resume", session_id),
    };
    args.extend([flag.to_string(), session_id.to_string()]);
    for (flag, value) in [
        ("--permission-prompt-tool", options.permission_prompt_tool),
        ("--mcp-config", options.mcp_config),
        ("--settings", options.settings),
        ("--permission-mode", options.permission_mode),
    ] {
        if let Some(value) = value {
            args.extend([flag.to_string(), value.to_string()]);
        }
    }
    for dir in options.add_dirs {
        args.extend(["--add-dir".to_string(), dir.display().to_string()]);
    }
    args
}
