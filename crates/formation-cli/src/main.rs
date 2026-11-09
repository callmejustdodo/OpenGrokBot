//! `formation`: the CLI. For now, the two entry points of the permission gate
//! (TASK-044) that Claude Code runs started by formationd use:
//!
//! - `formation mcp --permission`: an MCP stdio server with one `permission` tool,
//!   attached with `--permission-prompt-tool mcp__formation__permission`.
//! - `formation hook pre-tool-use`: the required `PreToolUse` hook.
//!
//! Both read the run and agent from `FORMATION_RUN_ID` and `FORMATION_AGENT_ID`,
//! send `permission_decide` to formationd over the local socket, and wait for its
//! answer. Anything that goes wrong denies the tool call (fail closed).

mod gate;

use std::io::Read;
use std::process::ExitCode;

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::Value;

use formation_proto::policy::{PermissionSource, PermissionVerdict, ToolCall};

const USAGE: &str = "usage:
  formation mcp --permission
  formation hook pre-tool-use";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["mcp", "--permission"] => run_permission_server(),
        ["hook", "pre-tool-use"] => run_pre_tool_use_hook(),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// What Claude Code sends to the permission tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PermissionInput {
    tool_name: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    tool_use_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PermissionServer {
    #[expect(dead_code, reason = "tool_handler macro reads this router field")]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl PermissionServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Ask Formation whether a Claude Code tool call may run")]
    async fn permission(&self, Parameters(request): Parameters<PermissionInput>) -> String {
        let call = ToolCall {
            tool_use_id: request.tool_use_id,
            tool_name: request.tool_name,
            input: request.input,
        };
        let verdict = gate::ask(PermissionSource::PermissionTool, call, None).await;
        // The permission tool always answers; `defer` is for the hook only.
        let verdict = match verdict {
            PermissionVerdict::Defer => PermissionVerdict::Deny {
                message: "Formation made no decision for this tool call.".into(),
            },
            verdict => verdict,
        };
        serde_json::to_string(&verdict).unwrap_or_else(|_| {
            r#"{"behavior":"deny","message":"Formation could not encode its answer."}"#.into()
        })
    }
}

#[tool_handler]
impl ServerHandler for PermissionServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn run_permission_server() -> ExitCode {
    let result = runtime().map_err(Into::into).and_then(|runtime| {
        runtime.block_on(async {
            let service = PermissionServer::new()
                .serve(rmcp::transport::stdio())
                .await?;
            service.waiting().await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        })
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("formation: permission server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_pre_tool_use_hook() -> ExitCode {
    let mut stdin = String::new();
    let verdict = match std::io::stdin().read_to_string(&mut stdin) {
        Err(error) => gate::deny(format!("cannot read the hook input: {error}")),
        Ok(_) => match serde_json::from_str::<Value>(&stdin) {
            Err(error) => gate::deny(format!("the hook input is not JSON: {error}")),
            Ok(payload) => match runtime() {
                Err(error) => gate::deny(format!("cannot start: {error}")),
                Ok(runtime) => {
                    let (call, session_id) = gate::hook_call(&payload);
                    runtime.block_on(gate::ask(
                        PermissionSource::PreToolUseHook,
                        call,
                        session_id,
                    ))
                }
            },
        },
    };
    if let Some(output) = gate::hook_output(&verdict) {
        println!("{output}");
    }
    ExitCode::SUCCESS
}
