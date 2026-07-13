use adk_agent::LlmAgentBuilder;
use adk_computer_use::{
    ComputerUseMcpConfig, ComputerUseMcpRuntime, ComputerUseRuntime, ScopeAuthorizer,
    TraceCorrelation, build_reference_graph_with_checkpointer,
};
use adk_core::{Agent, Content, Part};
use adk_graph::{ExecutionConfig, GraphError, MemoryCheckpointer, State};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::McpToolset;
use chrono::Utc;
use futures::StreamExt;
use rmcp::{ServiceExt, transport::TokioChildProcess};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

struct DemoPathCleanup(Vec<PathBuf>);

impl Drop for DemoPathCleanup {
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

fn output(value: &Value) -> &Value {
    value
        .get("response")
        .and_then(|value| value.get("output"))
        .or_else(|| value.get("output"))
        .unwrap_or(value)
}

fn nested<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    output(value).get(key).or_else(|| output(value).get("output").and_then(|value| value.get(key)))
}

fn object(value: Value) -> Result<Map<String, Value>, Box<dyn std::error::Error>> {
    value.as_object().cloned().ok_or_else(|| "expected an object".into())
}

async fn plan_fields(prompt: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .map_err(|_| "GOOGLE_API_KEY or GEMINI_API_KEY is required")?;
    let model_name =
        std::env::var("COMPUTER_USE_PLANNER_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());
    let schema = json!({
        "type": "object",
        "required": ["name", "project"],
        "properties": {
            "name": { "type": "string", "minLength": 1, "maxLength": 120 },
            "project": { "type": "string", "minLength": 1, "maxLength": 120 }
        }
    });
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("computer-use-v8-form-planner")
            .description("Schema-constrained planner for the governed form showcase")
            .instruction(
                "Extract the exact public demonstration Name and Project requested by the user. \
                 Return only schema-valid JSON. Do not add fields, tools, targets, or secrets. \
                 The downstream ADK graph and v8 runtime own approval and execution.",
            )
            .model(Arc::new(GeminiModel::new(&api_key, &model_name)?))
            .output_schema(schema)
            .output_max_retries(2)
            .temperature(0.0)
            .build()?,
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "computer-use-v8-form".into(),
            user_id: "local-operator".into(),
            session_id: Some("form-planner".into()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("computer-use-v8-form")
        .agent(agent)
        .session_service(sessions)
        .build()?;
    let mut stream = runner
        .run_str("local-operator", "form-planner", Content::new("user").with_text(prompt))
        .await?;
    let mut response = String::new();
    while let Some(event) = stream.next().await {
        if let Some(content) = event?.llm_response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    response.push_str(&text);
                }
            }
        }
    }
    let value: Value = serde_json::from_str(response.trim())?;
    if value.get("name").and_then(Value::as_str).is_none()
        || value.get("project").and_then(Value::as_str).is_none()
    {
        return Err("planner did not return the required public form fields".into());
    }
    Ok(value)
}

fn build_form_app(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let app = root.join("ADK Form Showcase.app");
    let contents = app.join("Contents");
    let executable_dir = contents.join("MacOS");
    fs::create_dir_all(&executable_dir)?;
    fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>ADKFormShowcase</string>
<key>CFBundleIdentifier</key><string>ai.zavora.adk-form-showcase</string>
<key>CFBundleName</key><string>ADK Form Showcase</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>"#,
    )?;
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join("macos_form_showcase.swift");
    let executable = executable_dir.join("ADKFormShowcase");
    let result = std::process::Command::new("swiftc")
        .arg(source)
        .args(["-o"])
        .arg(&executable)
        .args(["-framework", "AppKit"])
        .status()?;
    if !result.success() {
        return Err("swiftc failed to build the local form showcase".into());
    }
    Ok(app)
}

fn supervisor_dir(entrypoint: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(value) = std::env::var("COMPUTER_USE_SUPERVISOR_DIR") {
        return Ok(PathBuf::from(value));
    }
    let root = Path::new(entrypoint)
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot derive computer-use-mcp root from entrypoint")?;
    Ok(root.join("packages/computer-use-supervisor"))
}

fn spawn_pip(
    directory: &Path,
    socket: &Path,
    token: &str,
    principal: &str,
    session_id: &str,
) -> Result<Child, Box<dyn std::error::Error>> {
    let mut command = if let Ok(electron) = std::env::var("COMPUTER_USE_ELECTRON") {
        let mut command = Command::new(electron);
        command.arg(".");
        command
    } else {
        let mut command = Command::new("npx");
        command.args(["--yes", "--package=electron@43.1.0", "electron", "."]);
        command
    };
    Ok(command
        .current_dir(directory)
        .env("COMPUTER_USE_SUPERVISOR_SOCKET", socket)
        .env("COMPUTER_USE_SUPERVISOR_TOKEN", token)
        .env("COMPUTER_USE_PRINCIPAL_ID", principal)
        .env("COMPUTER_USE_SESSION_ID", session_id)
        .env("COMPUTER_USE_SUPERVISOR_DEBUG", "true")
        .stdout(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(not(target_os = "macos")) {
        return Err("the live native form showcase currently targets macOS".into());
    }
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let prompt = if prompt.is_empty() {
        "Use the public demo Name 'James' and Project 'computer-use v8 showcase'.".to_string()
    } else {
        prompt
    };
    println!("PROMPT: {prompt}");
    let fields = plan_fields(&prompt).await?;
    println!("PLANNED_PUBLIC_FIELDS: {}", serde_json::to_string(&fields)?);

    let run_root = std::env::temp_dir().join(format!("adk-v8-form-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&run_root)?;
    let app = build_form_app(&run_root)?;
    let socket = PathBuf::from(format!("/tmp/adk-v8-{}.sock", uuid::Uuid::new_v4().simple()));
    let _path_cleanup = DemoPathCleanup(vec![socket.clone(), run_root.clone()]);
    let mut form = Command::new(app.join("Contents/MacOS/ADKFormShowcase"));
    let mut form = form.kill_on_drop(true).spawn()?;

    let entrypoint = std::env::var("COMPUTER_USE_MCP_ENTRYPOINT")
        .map_err(|_| "COMPUTER_USE_MCP_ENTRYPOINT must point to the local dist/server.js")?;
    let principal =
        std::env::var("COMPUTER_USE_PRINCIPAL_ID").unwrap_or_else(|_| "adk-local-operator".into());
    // Darwin limits AF_UNIX paths to roughly 104 bytes. Keep the control
    // socket short even when the temporary app bundle lives in a long path.
    let supervisor_token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let mut server = Command::new(std::env::var("NODE").unwrap_or_else(|_| "node".into()));
    server
        .arg(&entrypoint)
        .env("COMPUTER_USE_V8", "true")
        .env("COMPUTER_USE_ACTIVE_PROFILE", "v8-safe")
        .env("COMPUTER_USE_PRINCIPAL_ID", &principal)
        .env("COMPUTER_USE_REQUIRE_APPROVAL_FOR", "fill_form")
        .env("COMPUTER_USE_RUNTIME_DEBUG", "true")
        .env("COMPUTER_USE_SUPERVISOR_SOCKET", &socket)
        .env("COMPUTER_USE_SUPERVISOR_TOKEN", &supervisor_token)
        .env("COMPUTER_USE_SUPERVISOR_FRAMES", "true");
    let client = ().serve(TokioChildProcess::new(server)?).await?;
    let toolset = Arc::new(McpToolset::new(client).with_name("computer-use-v8-form"));
    let started = toolset.call_tool_value("start_session", Map::new()).await?;
    let session_id = output(&started)
        .get("session")
        .and_then(|value| value.get("sessionId"))
        .and_then(Value::as_str)
        .ok_or("start_session did not return sessionId")?
        .to_string();

    let bootstrap = Arc::new(ComputerUseMcpRuntime::new(
        toolset.clone(),
        ComputerUseMcpConfig {
            session_id: session_id.clone(),
            expected_principal_id: principal.clone(),
            capability_tool: "fill_form".into(),
            target_app: None,
            target_window_id: None,
            correlation: TraceCorrelation::default(),
        },
    ));
    let target_window = timeout(Duration::from_secs(20), async {
        loop {
            let observed = bootstrap.observe_tool("list_windows", json!({})).await?;
            if let Some(window) =
                nested(&observed, "windows").and_then(Value::as_array).and_then(|windows| {
                    windows.iter().find(|window| {
                        window.get("title").and_then(Value::as_str) == Some("ADK Form Showcase")
                    })
                })
            {
                return Ok::<Value, String>(window.clone());
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "timed out discovering the showcase window")??;
    let window_id = target_window
        .get("windowId")
        .and_then(Value::as_u64)
        .ok_or("showcase window did not expose windowId")?;
    let app_id = target_window
        .get("bundleId")
        .and_then(Value::as_str)
        .ok_or("showcase window did not expose bundleId")?
        .to_string();
    let pid = target_window.get("pid").and_then(Value::as_u64).ok_or("window PID missing")?;
    println!("TARGET: app={app_id} pid={pid} window={window_id}");

    let mut pip = spawn_pip(
        &supervisor_dir(&entrypoint)?,
        &socket,
        &supervisor_token,
        &principal,
        &session_id,
    )?;
    let action_id = uuid::Uuid::new_v4().to_string();
    let proposed_action = json!({
        "action_id": action_id,
        "expires_in_ms": 300_000,
        "execution_group_id": "adk-form-showcase",
        "agent_id": "sole-form-executor",
        "tool": "fill_form",
        "arguments": {
            "window_id": window_id,
            "focus_strategy": "prepare_display",
            "fields": [
                { "role": "AXTextField", "label": "Name", "value": fields["name"] },
                { "role": "AXTextField", "label": "Project", "value": fields["project"] }
            ]
        },
        "mode": "foreground",
        "data_labels": ["public"],
        "target": {
            "platform": "darwin",
            "app_id": app_id,
            "pid": pid,
            "window_id": window_id,
            "bounds": target_window["bounds"],
            "observation_id": uuid::Uuid::new_v4().to_string(),
            "confidence": 1.0,
            "captured_at": Utc::now().to_rfc3339()
        }
    });
    println!("PLANNED_ACTION: {}", serde_json::to_string(&proposed_action)?);

    let runtime = Arc::new(ComputerUseMcpRuntime::new(
        toolset.clone(),
        ComputerUseMcpConfig {
            session_id: session_id.clone(),
            expected_principal_id: principal.clone(),
            capability_tool: "fill_form".into(),
            target_app: Some(app_id),
            target_window_id: Some(window_id),
            correlation: TraceCorrelation {
                adk_session_id: Some("adk-live-form".into()),
                adk_invocation_id: Some("adk-live-form-invocation".into()),
                adk_graph_thread_id: Some("adk-live-form-graph".into()),
                trace_id: None,
            },
        },
    ));
    let graph = build_reference_graph_with_checkpointer(
        runtime.clone(),
        Arc::new(ScopeAuthorizer::from_verified_identity(
            principal,
            None,
            ["computer:plan", "computer:execute:foreground"],
        )),
        Some(Arc::new(MemoryCheckpointer::new())),
    )?;
    let mut input = State::new();
    input.insert("proposed_action".into(), proposed_action.clone());
    let interrupted = match graph.invoke(input, ExecutionConfig::new("adk-live-form-graph")).await {
        Err(GraphError::Interrupted(interrupted)) => interrupted,
        Ok(_) => return Err("form action unexpectedly bypassed approval".into()),
        Err(error) => return Err(error.into()),
    };
    let preview = interrupted
        .state
        .get("preview")
        .cloned()
        .ok_or("interrupted graph did not retain its preview")?;
    let action_digest = preview
        .pointer("/envelope/argsDigest")
        .and_then(Value::as_str)
        .ok_or("preview action digest missing")?
        .to_string();
    let policy_digest = preview
        .pointer("/policy/policyDigest")
        .and_then(Value::as_str)
        .ok_or("preview policy digest missing")?
        .to_string();
    println!("APPROVAL_INTERRUPT: review the action in the PiP window");
    println!("APPROVAL_OPTIONS: exact action or same fields (10 uses / 2 minutes)");

    let approval_timeout = std::env::var("COMPUTER_USE_FORM_APPROVAL_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (30..=900).contains(value))
        .unwrap_or(300);
    timeout(Duration::from_secs(approval_timeout), async {
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
                        && event.get("actionId").and_then(Value::as_str) == Some(&action_id)
                    {
                        return Ok::<(), Box<dyn std::error::Error>>(());
                    }
                }
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "approval timed out")??;

    let approved_preview = runtime.preview_action(proposed_action.clone()).await?;
    if !approved_preview.executable {
        return Err("v8 did not recognize the runtime-held PiP approval".into());
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
            ExecutionConfig::new("adk-live-form-graph")
                .with_resume_from(&interrupted.checkpoint_id),
        )
        .await?;
    println!(
        "GRAPH_RESULT: {}",
        serde_json::to_string(&json!({
            "observations_joined": result.get("observations_joined"),
            "route": result.get("route"),
            "receipt_status": result.get("receipt").and_then(|value| value.get("status")),
            "receipt_id": result.get("receipt").and_then(|value| value.get("receiptId")),
            "verified": result.get("verified"),
            "runtime_held_approval": true
        }))?
    );
    println!("MACOS_VERIFICATION: v8 independently read back both form fields");
    sleep(Duration::from_secs(3)).await;
    let _ = pip.kill().await;
    let _ = form.kill().await;
    toolset.cancellation_token().await.cancel();
    Ok(())
}
