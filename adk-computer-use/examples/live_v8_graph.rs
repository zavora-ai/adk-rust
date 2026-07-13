use adk_computer_use::{
    ComputerUseMcpConfig, ComputerUseMcpRuntime, ScopeAuthorizer, TraceCorrelation,
    build_reference_graph,
};
use adk_graph::{ExecutionConfig, State};
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use tokio::process::Command;

fn output(value: &Value) -> &Value {
    value
        .get("response")
        .and_then(|value| value.get("output"))
        .or_else(|| value.get("output"))
        .unwrap_or(value)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let principal =
        std::env::var("COMPUTER_USE_PRINCIPAL_ID").unwrap_or_else(|_| "adk-local-operator".into());
    let package = std::env::var("COMPUTER_USE_MCP_PACKAGE")
        .unwrap_or_else(|_| "@zavora-ai/computer-use-mcp".into());
    let mut command = Command::new("npx");
    command
        .args(["--yes", "--prefer-offline", &package])
        .env("COMPUTER_USE_V8", "true")
        .env("COMPUTER_USE_ACTIVE_PROFILE", "v8-safe")
        .env("COMPUTER_USE_PRINCIPAL_ID", &principal);
    let client = ().serve(TokioChildProcess::new(command)?).await?;
    let toolset = Arc::new(McpToolset::new(client).with_name("computer-use-v8"));

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
                adk_graph_thread_id: Some("adk-live-v8".into()),
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
    input.insert(
        "proposed_action".into(),
        json!({
            "action_id": uuid::Uuid::new_v4().to_string(),
            "tool": "write_clipboard",
            "arguments": { "text": "ADK-Rust v8 live graph completed" },
            "mode": "background",
            "data_labels": ["public"]
        }),
    );
    let result = graph.invoke(input, ExecutionConfig::new("adk-live-v8")).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    toolset.cancellation_token().await.cancel();
    Ok(())
}
