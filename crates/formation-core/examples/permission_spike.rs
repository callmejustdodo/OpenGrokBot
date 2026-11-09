//! TASK-005 spike entry point: `mcp --permission` serves the Claude Code
//! `--permission-prompt-tool`, `hook pre-tool-use` answers the injected hook.
//! Environment variables are documented in `formation_core::harness::permission`.

use std::io::Read;
use std::process::ExitCode;

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Map, Value};

use formation_core::harness::permission::{
    self as spike, DECISION_ENV, Decision, HOOK_DECISION_ENV,
};

const USAGE: &str = "usage: permission_spike mcp --permission | permission_spike hook pre-tool-use";

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

/// Arguments Claude Code passes to the `--permission-prompt-tool`. Unknown
/// fields are kept so the log shows the full call.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct PermissionRequest {
    tool_name: String,
    #[serde(default)]
    input: Value,
    #[serde(flatten)]
    extra: Map<String, Value>,
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

    #[tool(description = "Decide a Claude Code permission prompt (Formation spike)")]
    async fn permission(&self, Parameters(request): Parameters<PermissionRequest>) -> String {
        let decision = Decision::parse(std::env::var(DECISION_ENV).ok().as_deref());
        let answer = spike::permission_tool_response(decision, &request.input);
        let mut raw = request.extra.clone();
        raw.insert("tool_name".into(), Value::String(request.tool_name.clone()));
        raw.insert("input".into(), request.input.clone());
        spike::log_call(
            "permission_tool",
            &request.tool_name,
            &request.input,
            &Value::Object(raw),
            &answer,
        );
        answer.to_string()
    }
}

#[tool_handler]
impl ServerHandler for PermissionServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}

fn run_permission_server() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("permission_spike: cannot start runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        let service = PermissionServer::new()
            .serve(rmcp::transport::stdio())
            .await?;
        service.waiting().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("permission_spike: permission server failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_pre_tool_use_hook() -> ExitCode {
    let mut stdin = String::new();
    if let Err(err) = std::io::stdin().read_to_string(&mut stdin) {
        eprintln!("permission_spike: cannot read hook input: {err}");
        return ExitCode::FAILURE;
    }
    let payload: Value = match serde_json::from_str(&stdin) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!("permission_spike: hook input is not JSON: {err}");
            return ExitCode::FAILURE;
        }
    };
    let decision = spike::hook_decision(
        std::env::var(HOOK_DECISION_ENV).ok().as_deref(),
        std::env::var(DECISION_ENV).ok().as_deref(),
    );
    let answer = decision.map_or(Value::Null, spike::hook_response);
    let tool_name = payload["tool_name"].as_str().unwrap_or_default();
    spike::log_call(
        "pre_tool_use_hook",
        tool_name,
        &payload["tool_input"],
        &payload,
        &answer,
    );
    if decision.is_some() {
        println!("{answer}");
    }
    ExitCode::SUCCESS
}
