use adk_computer_use::{
    ComputerUseMcpConfig, ComputerUseMcpRuntime, ComputerUseRuntime, ScopeAuthorizer,
    TraceCorrelation, build_reference_graph_with_checkpointer,
};
use adk_graph::{ExecutionConfig, GraphError, MemoryCheckpointer, State};
use adk_tool::McpToolset;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

const FINDER_APP: &str = "com.apple.finder";
const FINDER_OPERATION: &str = "finder_set_sandbox_file_comment";

struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[path = "../support/mod.rs"]
mod support;
use support::{object, output, spawn_pip, supervisor_dir};

fn nested<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(found) = value.get(key) {
        return Some(found);
    }
    match value {
        Value::Array(values) => values.iter().find_map(|value| nested(value, key)),
        Value::Object(values) => values.values().find_map(|value| nested(value, key)),
        _ => None,
    }
}

fn apple_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"").replace('\r', "\\r").replace('\n', "\\n")
    )
}

fn finder_action(path: &Path, comment: &str, certification_id: &str) -> Value {
    let path = path.to_string_lossy();
    let script = format!(
        "tell application id \"com.apple.Finder\" to set comment of (POSIX file {} as alias) to {}",
        apple_string(&path),
        apple_string(comment),
    );
    json!({
        "action_id": uuid::Uuid::new_v4().to_string(),
        "execution_group_id": "adk-background-finder-demo",
        "agent_id": "background-finder-executor",
        "tool": "run_script",
        "operation": FINDER_OPERATION,
        "certification_id": certification_id,
        "arguments": {
            "language": "applescript",
            "script": script,
            "timeout_ms": 10_000,
            "certified_path": path,
            "certified_comment": comment,
            "target_app": FINDER_APP
        },
        "mode": "background",
        "data_labels": ["public"],
        "expires_in_ms": 300_000
    })
}

async fn wait_for_approval<S>(
    toolset: &McpToolset<S>,
    session_id: &str,
    action_id: &str,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: rmcp::service::Service<rmcp::RoleClient> + Send + Sync + 'static,
{
    timeout(Duration::from_secs(300), async {
        let mut after = 0_u64;
        loop {
            let events = toolset
                .call_tool_value(
                    "get_session_events",
                    object(json!({
                        "session_id": session_id,
                        "after_sequence": after,
                        "limit": 100
                    }))?,
                )
                .await?;
            if let Some(items) = nested(&events, "events").and_then(Value::as_array) {
                for event in items {
                    after = after.max(event.get("sequence").and_then(Value::as_u64).unwrap_or(0));
                    if event.get("type").and_then(Value::as_str) == Some("action.approved")
                        && event.get("actionId").and_then(Value::as_str) == Some(action_id)
                    {
                        return Ok::<(), Box<dyn std::error::Error>>(());
                    }
                }
            }
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| "approval timed out")??;
    Ok(())
}

async fn desktop_state<S>(
    runtime: &ComputerUseMcpRuntime<S>,
) -> Result<Value, Box<dyn std::error::Error>>
where
    S: rmcp::service::Service<rmcp::RoleClient> + Send + Sync + 'static,
{
    let front = runtime.observe_tool("get_frontmost_app", json!({})).await?;
    let pointer = runtime.observe_tool("cursor_position", json!({})).await?;
    let serialized = serde_json::to_string(&pointer)?;
    let coordinates = serialized
        .split('(')
        .nth(1)
        .and_then(|value| value.split(')').next())
        .unwrap_or("unknown")
        .to_string();
    Ok(json!({
        "frontmost_app": nested(output(&front), "bundleId").and_then(Value::as_str),
        "pointer": coordinates
    }))
}

#[allow(clippy::too_many_arguments)]
async fn run_approved_action<S>(
    toolset: Arc<McpToolset<S>>,
    runtime: Arc<ComputerUseMcpRuntime<S>>,
    principal: &str,
    session_id: &str,
    supervisor_path: &Path,
    socket: &Path,
    token: &str,
    proposed_action: Value,
    label: &str,
) -> Result<(State, Value, Value), Box<dyn std::error::Error>>
where
    S: rmcp::service::Service<rmcp::RoleClient> + Send + Sync + 'static,
{
    let graph = build_reference_graph_with_checkpointer(
        runtime.clone(),
        Arc::new(ScopeAuthorizer::from_verified_identity(
            principal,
            None,
            ["computer:plan", "computer:execute:background"],
        )),
        Some(Arc::new(MemoryCheckpointer::new())),
    )?;
    let thread_id = format!("adk-background-{label}-{}", uuid::Uuid::new_v4());
    let mut input = State::new();
    input.insert("proposed_action".into(), proposed_action.clone());
    let interrupted = match graph.invoke(input, ExecutionConfig::new(&thread_id)).await {
        Err(GraphError::Interrupted(interrupted)) => interrupted,
        Ok(_) => return Err(format!("{label} unexpectedly bypassed approval").into()),
        Err(error) => return Err(error.into()),
    };
    let preview = interrupted.state.get("preview").ok_or("interrupt lost preview")?;
    let action_digest = preview
        .pointer("/envelope/argsDigest")
        .and_then(Value::as_str)
        .ok_or("action digest missing")?;
    let policy_digest = preview
        .pointer("/policy/policyDigest")
        .and_then(Value::as_str)
        .ok_or("policy digest missing")?;
    let action_id =
        proposed_action.get("action_id").and_then(Value::as_str).ok_or("action id missing")?;
    let mut pip = spawn_pip(supervisor_path, socket, token, principal, session_id)?;
    println!("{label}_APPROVAL: choose 'Allow this once' in PiP");
    wait_for_approval(&toolset, session_id, action_id).await?;
    pip.kill().await?;
    sleep(Duration::from_millis(600)).await;
    let before = desktop_state(&runtime).await?;
    if !runtime.preview_action(proposed_action).await?.executable {
        return Err("runtime-held approval was not executable".into());
    }
    let mut resume = State::new();
    resume.insert(
        "approval".into(),
        json!({
            "actionDigest": action_digest,
            "policyDigest": policy_digest,
            "runtimeApproved": true
        }),
    );
    let result = graph
        .invoke(
            resume,
            ExecutionConfig::new(&thread_id).with_resume_from(&interrupted.checkpoint_id),
        )
        .await?;
    let after = desktop_state(&runtime).await?;
    Ok((result, before, after))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "macos")) {
        return Err("the Finder background showcase requires macOS".into());
    }
    let certification_id = std::env::var("COMPUTER_USE_FINDER_CERTIFICATION_ID")
        .map_err(|_| "COMPUTER_USE_FINDER_CERTIFICATION_ID is required")?;
    let entrypoint = std::env::var("COMPUTER_USE_MCP_ENTRYPOINT")
        .map_err(|_| "COMPUTER_USE_MCP_ENTRYPOINT must point to dist/server.js")?;
    let principal =
        std::env::var("COMPUTER_USE_PRINCIPAL_ID").unwrap_or_else(|_| "adk-local-operator".into());
    let sandbox = std::env::var("COMPUTER_USE_CERTIFICATION_SANDBOX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap())
                .join(".computer-use-mcp/certification-sandbox")
        });
    fs::create_dir_all(&sandbox)?;
    let demo_dir = sandbox.join(format!("adk-background-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&demo_dir)?;
    let demo_file = demo_dir.join("background-demo.txt");
    fs::write(&demo_file, "ADK background Finder comment demo\n")?;
    let socket = PathBuf::from(format!("/tmp/adk-bg-{}.sock", uuid::Uuid::new_v4().simple()));
    let _cleanup = Cleanup(vec![socket.clone(), demo_dir]);
    let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());

    let mut server = Command::new(std::env::var("NODE").unwrap_or_else(|_| "node".into()));
    server
        .arg(&entrypoint)
        .env("COMPUTER_USE_V8", "true")
        .env("COMPUTER_USE_ACTIVE_PROFILE", "v8-safe")
        .env("COMPUTER_USE_PRINCIPAL_ID", &principal)
        .env("COMPUTER_USE_SUPERVISOR_SOCKET", &socket)
        .env("COMPUTER_USE_SUPERVISOR_TOKEN", &token)
        .env("COMPUTER_USE_SUPERVISOR_FRAMES", "true")
        .env("COMPUTER_USE_RUNTIME_DEBUG", "true");
    let client = ().serve(TokioChildProcess::new(server)?).await?;
    let toolset = Arc::new(McpToolset::new(client).with_name("adk-background-finder"));
    let started = toolset
        .call_tool_value(
            "start_session",
            object(json!({
                "objective": "Update a Finder file comment in the background, prove Codex and the physical pointer stay undisturbed, then roll the comment back."
            }))?,
        )
        .await?;
    let session_id = nested(&started, "sessionId")
        .and_then(Value::as_str)
        .ok_or("session id missing")?
        .to_string();
    let runtime = Arc::new(ComputerUseMcpRuntime::new(
        toolset.clone(),
        ComputerUseMcpConfig {
            session_id: session_id.clone(),
            expected_principal_id: principal.clone(),
            capability_tool: "run_script".into(),
            target_app: Some(FINDER_APP.into()),
            target_window_id: None,
            correlation: TraceCorrelation {
                adk_session_id: Some("adk-background-finder".into()),
                adk_invocation_id: Some("adk-background-finder-live".into()),
                adk_graph_thread_id: Some("adk-background-finder-graph".into()),
                trace_id: None,
            },
        },
    ));

    let capabilities = toolset
        .call_tool_value(
            "get_execution_capabilities",
            object(json!({ "tool": "run_script", "app_id": FINDER_APP }))?,
        )
        .await?;
    let trace = toolset
        .call_tool_value(
            "get_certification_trace",
            object(json!({ "certification_id": certification_id }))?,
        )
        .await?;
    let trace = nested(&trace, "trace").ok_or("certification trace was not restored")?;
    let live_certificate = json!({
        "certification_id": trace.get("certificationId"),
        "trace_digest": trace.get("traceDigest"),
        "valid_until": trace.pointer("/capability/certification/validUntil"),
        "supported_modes": trace.pointer("/capability/supportedModes"),
        "interference": trace.pointer("/capability/interference"),
        "frontmost_before": trace.pointer("/probe/result/evidence/frontmostAppBefore"),
        "frontmost_after": trace.pointer("/probe/result/evidence/frontmostAppAfter"),
        "pointer_before": trace.pointer("/probe/result/evidence/pointerBefore"),
        "pointer_after": trace.pointer("/probe/result/evidence/pointerAfter"),
        "physical_input_injected": trace.pointer("/probe/result/physicalInputInjected"),
        "rollback_succeeded": trace.pointer("/probe/result/evidence/rollbackSucceeded")
    });
    println!("LIVE_CAPABILITY_CERTIFICATE: {}", serde_json::to_string(&live_certificate)?);
    if !serde_json::to_string(&capabilities)?.contains(&certification_id) {
        return Err("fresh certificate is not active in the MCP capability registry".into());
    }

    let supervisor = supervisor_dir(&entrypoint)?;
    let marker = "ADK-Rust certified background update";
    let update = finder_action(&demo_file, marker, &certification_id);
    let (update_result, before, after) = run_approved_action(
        toolset.clone(),
        runtime.clone(),
        &principal,
        &session_id,
        &supervisor,
        &socket,
        &token,
        update,
        "UPDATE",
    )
    .await?;
    println!(
        "BACKGROUND_UPDATE: {}",
        serde_json::to_string(&json!({
            "mode": update_result.get("lease").and_then(|value| value.get("executionMode")),
            "receipt_status": update_result.get("receipt").and_then(|value| value.get("status")),
            "verified": update_result.get("verified"),
            "before": before,
            "after": after,
            "focus_unchanged": before.get("frontmost_app") == after.get("frontmost_app"),
            "pointer_unchanged": before.get("pointer") == after.get("pointer")
        }))?
    );

    let rollback = finder_action(&demo_file, "", &certification_id);
    let (rollback_result, rollback_before, rollback_after) = run_approved_action(
        toolset.clone(),
        runtime,
        &principal,
        &session_id,
        &supervisor,
        &socket,
        &token,
        rollback,
        "ROLLBACK",
    )
    .await?;
    let readback = Command::new("osascript")
        .args([
            "-e",
            &format!(
                "tell application id \"com.apple.Finder\" to get comment of (POSIX file {} as alias)",
                apple_string(&demo_file.to_string_lossy())
            ),
        ])
        .output()
        .await?;
    let restored =
        readback.status.success() && String::from_utf8(readback.stdout)?.trim().is_empty();
    println!(
        "VERIFIED_ROLLBACK: {}",
        serde_json::to_string(&json!({
            "receipt_status": rollback_result.get("receipt").and_then(|value| value.get("status")),
            "verified": rollback_result.get("verified"),
            "original_comment_restored": restored,
            "before": rollback_before,
            "after": rollback_after,
            "focus_unchanged": rollback_before.get("frontmost_app") == rollback_after.get("frontmost_app"),
            "pointer_unchanged": rollback_before.get("pointer") == rollback_after.get("pointer")
        }))?
    );
    if !restored {
        return Err("Finder comment rollback readback failed".into());
    }
    toolset.cancellation_token().await.cancel();
    Ok(())
}
