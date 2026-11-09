//! Parser for `claude -p --output-format stream-json --verbose
//! --include-partial-messages` output: one JSON object per line.
//!
//! Only the fields the answer runner needs are read, so new or renamed fields
//! elsewhere in the stream do not break it.

use serde_json::Value;

/// One stream-json line, reduced to what the answer runner needs.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// `{"type":"system","subtype":"init"}`. With `--fork-session` the session
    /// ID is already the fork's new ID. `tools` and `mcp_servers` (server
    /// names) are what the run can use; `None` when the event leaves them out.
    Init {
        session_id: Option<String>,
        tools: Option<Vec<String>>,
        mcp_servers: Option<Vec<String>>,
    },
    /// Answer text from a `content_block_delta` whose delta is a `text_delta`.
    /// Thinking blocks always come first and carry no text.
    TextDelta(String),
    /// The final `{"type":"result"}` event.
    Result(StreamResult),
    /// Anything else: status, thinking, signatures, rate limits.
    Other,
}

/// The fields of the `result` event that the answer runner keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamResult {
    pub session_id: Option<String>,
    /// `is_error`, or a `subtype` other than `success`. The CLI reports
    /// `subtype: "success"` even for a sign-in failure, so `is_error` decides.
    pub is_error: bool,
    /// The final answer text (`result`), or the error message.
    pub text: String,
    /// `completed` or `api_error`, when present.
    pub terminal_reason: Option<String>,
    /// The CLI's own time to first answer text (`ttft_ms`).
    pub cli_ttft_ms: Option<u64>,
    /// The CLI's own run time for this process (`duration_ms`).
    pub cli_duration_ms: Option<u64>,
    /// Model turns in this run (`num_turns`). A local command such as
    /// `/context` answers with 0 turns, without the model.
    pub num_turns: Option<u64>,
    /// Cost of this run only: the sum of `model_usage_since_last_result[*].costUSD`.
    /// `total_cost_usd` is not used because a resumed session adds the
    /// original session's cost to it.
    pub cost_usd: Option<f64>,
}

/// Parses one line of stream-json output.
pub fn parse_line(line: &str) -> Result<StreamEvent, serde_json::Error> {
    let value: Value = serde_json::from_str(line)?;
    Ok(classify(&value))
}

fn classify(value: &Value) -> StreamEvent {
    match value["type"].as_str() {
        Some("system") if value["subtype"] == "init" => StreamEvent::Init {
            session_id: value["session_id"].as_str().map(str::to_owned),
            tools: strings(&value["tools"], |tool| tool.as_str()),
            mcp_servers: strings(&value["mcp_servers"], |server| {
                server["name"].as_str().or_else(|| server.as_str())
            }),
        },
        Some("stream_event") => {
            let event = &value["event"];
            match event["delta"]["text"].as_str() {
                Some(text)
                    if event["type"] == "content_block_delta"
                        && event["delta"]["type"] == "text_delta" =>
                {
                    StreamEvent::TextDelta(text.to_owned())
                }
                _ => StreamEvent::Other,
            }
        }
        Some("result") => StreamEvent::Result(StreamResult {
            session_id: value["session_id"].as_str().map(str::to_owned),
            is_error: value["is_error"].as_bool().unwrap_or(false)
                || value["subtype"].as_str().is_some_and(|s| s != "success"),
            text: value["result"].as_str().unwrap_or_default().to_owned(),
            terminal_reason: value["terminal_reason"].as_str().map(str::to_owned),
            cli_ttft_ms: value["ttft_ms"].as_u64(),
            cli_duration_ms: value["duration_ms"].as_u64(),
            num_turns: value["num_turns"].as_u64(),
            cost_usd: value["model_usage_since_last_result"]
                .as_object()
                .map(|models| models.values().filter_map(|m| m["costUSD"].as_f64()).sum()),
        }),
        _ => StreamEvent::Other,
    }
}

/// The strings `item` picks from each element of a JSON array, or `None` if
/// `value` is not an array.
fn strings(value: &Value, item: impl Fn(&Value) -> Option<&str>) -> Option<Vec<String>> {
    let items = value.as_array()?;
    Some(
        items
            .iter()
            .map(|v| item(v).map_or_else(|| v.to_string(), str::to_owned))
            .collect(),
    )
}

/// Everything learned from the lines of one run so far.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StreamSummary {
    /// From the `init` event, or from the result if `init` was missing.
    pub session_id: Option<String>,
    /// The tools the `init` event listed; `Some(empty)` when tools are off.
    pub tools: Option<Vec<String>>,
    /// The MCP servers the `init` event listed.
    pub mcp_servers: Option<Vec<String>>,
    /// The concatenated text deltas.
    pub text: String,
    pub result: Option<StreamResult>,
    /// Non-empty lines that were not JSON.
    pub skipped_lines: usize,
}

impl StreamSummary {
    /// Adds one stdout line and returns what it was.
    pub fn feed(&mut self, line: &str) -> StreamEvent {
        let line = line.trim();
        if line.is_empty() {
            return StreamEvent::Other;
        }
        let Ok(event) = parse_line(line) else {
            self.skipped_lines += 1;
            return StreamEvent::Other;
        };
        match &event {
            StreamEvent::Init {
                session_id,
                tools,
                mcp_servers,
            } => {
                if self.session_id.is_none() {
                    self.session_id.clone_from(session_id);
                }
                self.tools.clone_from(tools);
                self.mcp_servers.clone_from(mcp_servers);
            }
            StreamEvent::TextDelta(text) => self.text.push_str(text),
            StreamEvent::Result(result) => {
                if self.session_id.is_none() {
                    self.session_id.clone_from(&result.session_id);
                }
                self.result = Some(result.clone());
            }
            StreamEvent::Other => {}
        }
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sanitized fork answer with the event order recorded from Claude Code
    /// 2.1.319: init, status, thinking block, text deltas, rate limit, result.
    const FORK_ANSWER: &str = include_str!("testdata/fork_answer.jsonl");
    /// The stream `--bare` produced without an API key.
    const NOT_LOGGED_IN: &str = include_str!("testdata/not_logged_in.jsonl");

    const FORK_ID: &str = "11111111-2222-4333-8444-555555555555";

    fn summarize(stream: &str) -> StreamSummary {
        let mut summary = StreamSummary::default();
        for line in stream.lines() {
            summary.feed(line);
        }
        summary
    }

    #[test]
    fn fork_answer_yields_fork_id_text_and_run_figures() {
        let summary = summarize(FORK_ANSWER);
        assert_eq!(summary.session_id.as_deref(), Some(FORK_ID));
        assert_eq!(summary.skipped_lines, 0);
        assert_eq!(summary.tools, Some(vec![]));
        assert_eq!(summary.mcp_servers, Some(vec![]));

        let result = summary.result.expect("result event");
        assert!(!result.is_error);
        assert_eq!(result.session_id.as_deref(), Some(FORK_ID));
        assert_eq!(result.terminal_reason.as_deref(), Some("completed"));
        assert_eq!(result.cli_ttft_ms, Some(5028));
        assert_eq!(result.cli_duration_ms, Some(6839));
        assert_eq!(result.num_turns, Some(1));
        // The run's own cost, not total_cost_usd (0.1997186), which includes
        // the original session's cost.
        assert_eq!(result.cost_usd, Some(0.0679436));

        // The deltas add up to the final text.
        assert_eq!(summary.text, result.text);
        assert!(result.text.starts_with("The login API cleanup is done"));
        assert!(result.text.contains("Messages used:"));
    }

    #[test]
    fn thinking_and_signature_deltas_are_not_answer_text() {
        let thinking = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"","estimated_tokens":50}},"session_id":"s"}"#;
        let signature = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"c2ln"}},"session_id":"s"}"#;
        let text = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}},"session_id":"s"}"#;
        assert_eq!(parse_line(thinking).unwrap(), StreamEvent::Other);
        assert_eq!(parse_line(signature).unwrap(), StreamEvent::Other);
        assert_eq!(
            parse_line(text).unwrap(),
            StreamEvent::TextDelta("Hi".into())
        );
    }

    #[test]
    fn sign_in_failure_is_an_error_despite_success_subtype() {
        let summary = summarize(NOT_LOGGED_IN);
        let result = summary.result.expect("result event");
        assert!(result.is_error);
        assert_eq!(result.text, "Not logged in · Please run /login");
        assert_eq!(result.terminal_reason.as_deref(), Some("api_error"));
        assert_eq!(result.cli_ttft_ms, None);
        assert_eq!(result.cost_usd, Some(0.0));
        // The synthetic error message is not streamed as answer text.
        assert!(summary.text.is_empty());
    }

    #[test]
    fn error_subtype_counts_as_error() {
        let line = r#"{"type":"result","subtype":"error_during_execution","session_id":"s"}"#;
        let StreamEvent::Result(result) = parse_line(line).unwrap() else {
            panic!("expected a result event");
        };
        assert!(result.is_error);
        assert_eq!(result.cost_usd, None);
    }

    #[test]
    fn blank_and_non_json_lines_are_skipped() {
        let mut summary = StreamSummary::default();
        assert_eq!(summary.feed(""), StreamEvent::Other);
        assert_eq!(summary.feed("   "), StreamEvent::Other);
        assert_eq!(summary.feed("warning: something"), StreamEvent::Other);
        assert_eq!(summary.skipped_lines, 1);
    }

    #[test]
    fn init_keeps_the_tools_and_mcp_servers_it_lists() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s","tools":["Read","mcp__x__y"],"mcp_servers":[{"name":"x","status":"connected"}]}"#;
        let mut summary = StreamSummary::default();
        assert_eq!(
            summary.feed(line),
            StreamEvent::Init {
                session_id: Some("s".into()),
                tools: Some(vec!["Read".into(), "mcp__x__y".into()]),
                mcp_servers: Some(vec!["x".into()]),
            }
        );
        assert_eq!(summary.session_id.as_deref(), Some("s"));
        assert_eq!(summary.tools.as_deref().map(<[String]>::len), Some(2));

        // An init event without the lists says nothing about them.
        let bare = r#"{"type":"system","subtype":"init","session_id":"s"}"#;
        assert_eq!(
            parse_line(bare).unwrap(),
            StreamEvent::Init {
                session_id: Some("s".into()),
                tools: None,
                mcp_servers: None,
            }
        );
    }

    #[test]
    fn session_id_falls_back_to_the_result() {
        let mut summary = StreamSummary::default();
        summary.feed(r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"from-result"}"#);
        assert_eq!(summary.session_id.as_deref(), Some("from-result"));
    }
}
