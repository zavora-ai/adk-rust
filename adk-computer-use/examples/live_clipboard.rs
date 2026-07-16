//! Cross-platform live showcase: a natural-language prompt that ends up on the
//! real system clipboard, driven through the governed computer-use graph and
//! independently verified.
//!
//! This runs on **macOS, Linux, and Windows** — the `computer-use-mcp` server
//! performs the actual clipboard write, and this example verifies it by reading
//! the clipboard back with the platform's own tool (see
//! [`support::read_clipboard`]).
//!
//! ## Prerequisites
//!
//! - A running `computer-use-mcp` build. Point `COMPUTER_USE_MCP_ENTRYPOINT` at
//!   a local `dist/server.js`, or set `COMPUTER_USE_MCP_PACKAGE` to an npm
//!   specifier (defaults to `@zavora-ai/computer-use-mcp` via `npx`).
//! - **Node.js** (override the binary with `NODE`).
//! - A Gemini API key (`GOOGLE_API_KEY` or `GEMINI_API_KEY`) for the planner.
//! - Clipboard read-back tooling: nothing extra on macOS (`pbpaste`) or Windows
//!   (`Get-Clipboard`); on Linux install `wl-clipboard`, `xclip`, or `xsel`.
//!
//! ```bash
//! cargo run -p adk-computer-use --example live_clipboard -- \
//!   "Place the exact public text 'ADK-Rust live prompt completed' on my clipboard."
//! ```

use adk_agent::LlmAgentBuilder;
use adk_computer_use::{
    ComputerUseMcpConfig, ComputerUseMcpRuntime, ScopeAuthorizer, TraceCorrelation,
    build_reference_graph,
};
use adk_core::{Agent, Content, Part};
use adk_graph::{ExecutionConfig, State};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::McpToolset;
use futures::StreamExt;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;

#[path = "support/mod.rs"]
mod support;
use support::{output, read_clipboard};

async fn plan_prompt(prompt: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .map_err(|_| "GOOGLE_API_KEY or GEMINI_API_KEY is required for the live prompt planner")?;
    let model_name =
        std::env::var("COMPUTER_USE_PLANNER_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());
    let model = Arc::new(GeminiModel::new(&api_key, &model_name)?);
    let schema = json!({
        "type": "object",
        "required": ["tool", "arguments", "mode", "data_labels"],
        "properties": {
            "tool": { "type": "string", "enum": ["write_clipboard"] },
            "arguments": {
                "type": "object",
                "required": ["text"],
                "properties": { "text": { "type": "string", "minLength": 1, "maxLength": 4096 } }
            },
            "mode": { "type": "string", "enum": ["background"] },
            "data_labels": {
                "type": "array", "minItems": 1, "maxItems": 1,
                "items": { "type": "string", "enum": ["public"] }
            }
        }
    });
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("computer-use-planner")
            .description("Constrained natural-language planner for the governed showcase")
            .instruction(
                "Convert the user's request into the one permitted public demonstration action. \
                 Preserve the exact text the user asks to place on the clipboard. Do not invent \
                 another tool, target, mode, or data label. Return only schema-valid JSON. The \
                 downstream governed graph, not this planner, owns authorization and execution.",
            )
            .model(model)
            .output_schema(schema)
            .output_max_retries(2)
            .temperature(0.0)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "computer-use-live".into(),
            user_id: "local-operator".into(),
            session_id: Some("prompt-planner".into()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("computer-use-live")
        .agent(agent)
        .session_service(sessions)
        .build()?;
    let content = Content::new("user").with_text(prompt);
    let mut stream = runner.run_str("local-operator", "prompt-planner", content).await?;
    let mut response = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = event.llm_response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    response.push_str(&text);
                }
            }
        }
    }
    let planned: Value = serde_json::from_str(response.trim())?;
    if planned.get("tool").and_then(Value::as_str) != Some("write_clipboard")
        || planned.pointer("/arguments/text").and_then(Value::as_str).is_none()
        || planned.get("mode").and_then(Value::as_str) != Some("background")
        || planned.get("data_labels") != Some(&json!(["public"]))
    {
        return Err("planner output escaped the constrained showcase schema".into());
    }
    Ok(planned)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.is_empty() {
        "Place the exact public text 'ADK-Rust live prompt completed' on my clipboard.".to_string()
    } else {
        prompt
    };
    println!("PROMPT: {prompt}");
    let mut proposed_action = plan_prompt(&prompt).await?;
    proposed_action["action_id"] = Value::String(uuid::Uuid::new_v4().to_string());
    println!("PLANNED_ACTION: {}", serde_json::to_string(&proposed_action)?);

    let principal =
        std::env::var("COMPUTER_USE_PRINCIPAL_ID").unwrap_or_else(|_| "adk-local-operator".into());
    let mut command = if let Ok(entrypoint) = std::env::var("COMPUTER_USE_MCP_ENTRYPOINT") {
        let mut command = Command::new(std::env::var("NODE").unwrap_or_else(|_| "node".into()));
        command.arg(entrypoint);
        command
    } else {
        let package = std::env::var("COMPUTER_USE_MCP_PACKAGE")
            .unwrap_or_else(|_| "@zavora-ai/computer-use-mcp".into());
        let mut command = Command::new("npx");
        command.args(["--yes", "--prefer-offline", &package]);
        command
    };
    command
        .env("COMPUTER_USE_V8", "true")
        .env("COMPUTER_USE_ACTIVE_PROFILE", "v8-safe")
        .env("COMPUTER_USE_PRINCIPAL_ID", &principal);
    let client = ().serve(TokioChildProcess::new(command)?).await?;
    let toolset = Arc::new(McpToolset::new(client).with_name("computer-use"));

    let started = toolset.call_tool_value("start_session", Map::new()).await?;
    let session_id = output(&started)
        .get("session")
        .and_then(|session| session.get("sessionId"))
        .and_then(Value::as_str)
        .ok_or("start_session did not return sessionId")?
        .to_string();
    let runtime = Arc::new(ComputerUseMcpRuntime::new(
        toolset.clone(),
        ComputerUseMcpConfig {
            session_id,
            expected_principal_id: principal.clone(),
            capability_tool: "write_clipboard".into(),
            target_app: None,
            target_window_id: None,
            correlation: TraceCorrelation {
                adk_session_id: Some("adk-live-session".into()),
                adk_invocation_id: Some("adk-live-invocation".into()),
                adk_graph_thread_id: Some("adk-live".into()),
                trace_id: None,
            },
        },
    ));
    let graph = build_reference_graph(
        runtime,
        Arc::new(ScopeAuthorizer::from_verified_identity(
            principal,
            None,
            ["computer:plan", "computer:execute:background"],
        )),
    )?;
    let mut input = State::new();
    input.insert("proposed_action".into(), proposed_action.clone());
    let result = graph.invoke(input, ExecutionConfig::new("adk-live")).await?;
    let safe_result = json!({
        "observations_joined": result.get("observations_joined") == Some(&Value::Bool(true)),
        "route": result.get("route").and_then(Value::as_str),
        "lease_id": result.get("lease").and_then(|value| value.get("leaseId")).and_then(Value::as_str),
        "lease_mode": result.get("lease").and_then(|value| value.get("executionMode")).and_then(Value::as_str),
        "receipt_id": result.get("receipt").and_then(|value| value.get("receiptId")).and_then(Value::as_str),
        "receipt_status": result.get("receipt").and_then(|value| value.get("status")).and_then(Value::as_str),
        "action_digest": result.get("receipt").and_then(|value| value.get("actionDigest")).and_then(Value::as_str),
        "verified": result.get("verified").and_then(Value::as_bool),
        "result": result.get("result"),
    });
    println!("GRAPH_RESULT: {}", serde_json::to_string(&safe_result)?);
    let expected = proposed_action
        .pointer("/arguments/text")
        .and_then(Value::as_str)
        .ok_or("planned action lost clipboard text")?;
    let actual = read_clipboard().await?;
    if actual != expected {
        return Err(format!(
            "clipboard verification failed: expected {expected:?}, got {actual:?}"
        )
        .into());
    }
    println!("CLIPBOARD_VERIFICATION: clipboard matched planned text exactly");
    toolset.cancellation_token().await.cancel();
    Ok(())
}
