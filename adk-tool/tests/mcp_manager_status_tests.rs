//! `McpServerManager` status transitions and the stdio launcher (spec 3.2 §10 T1): a
//! subscriber sees a killed server crash and restart in order, a server whose restarts all
//! fail ends `FailedToStart`, and a launcher's command is the one the manager spawns. On
//! Windows, stopping a server ends the processes it started (spec 3.2 §10 W1).
//!
//! The stdio server is this test binary, re-run as a child with [`SERVER_ENV`] set. The
//! lines libtest prints before the server starts are not JSON-RPC, which the client skips.
#![cfg(feature = "mcp")]

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(unix)]
use adk_core::AdkError;
use adk_core::{ReadonlyContext, Toolset};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::{McpServerConfig, McpServerManager, RestartPolicy};
// The crash and restart tests kill the server with `kill -9`, so they run on Unix.
#[cfg(unix)]
use adk_tool::mcp::{ServerStatus, ServerStatusTransition};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
#[cfg(unix)]
use tokio::sync::broadcast;

/// Names the mode when this binary runs as the stdio MCP server.
const SERVER_ENV: &str = "ADK_TOOL_MCP_STATUS_TEST_SERVER";

/// Set by the launcher under test; the server reports it through `launch_tag`.
const TAG_ENV: &str = "ADK_TOOL_MCP_LAUNCH_TAG";

/// Long enough that only a missed transition reaches it.
#[cfg(unix)]
const DEADLINE: Duration = Duration::from_secs(30);

/// Publishes `server_pid`, returning this process's ID, `launch_tag`, returning
/// [`TAG_ENV`], and `spawn_descendant`, which starts a process that runs for five minutes on
/// Windows unless something ends it, and returns its ID.
#[derive(Clone)]
struct Probe;

impl ServerHandler for Probe {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(serde_json::Map::new());
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("server_pid", "Returns the server's process ID.", Arc::clone(&schema)),
            Tool::new("launch_tag", "Returns the tag the launcher set.", Arc::clone(&schema)),
            Tool::new("spawn_descendant", "Starts a long-running process.", schema),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let text = match params.name.as_ref() {
            "server_pid" => std::process::id().to_string(),
            "spawn_descendant" => {
                // Five minutes of `ping` on Windows, the platform whose tests call the tool.
                let mut child = std::process::Command::new("ping")
                    .args(["-n", "300", "127.0.0.1"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap();
                let pid = child.id();
                // Reaped in the background, so the server does not wait for it.
                std::thread::spawn(move || child.wait());
                pid.to_string()
            }
            _ => std::env::var(TAG_ENV).unwrap_or_else(|_| "untagged".to_string()),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

/// Serves [`Probe`] over stdio when [`SERVER_ENV`] is set.
#[tokio::test]
#[ignore = "runs only as the stdio MCP server the other tests start"]
async fn stdio_server() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = Probe.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

fn server_args() -> Vec<String> {
    ["stdio_server", "--exact", "--ignored", "--quiet", "--test-threads=1"]
        .map(str::to_string)
        .to_vec()
}

fn server_config(restart_policy: Option<RestartPolicy>) -> McpServerConfig {
    McpServerConfig {
        command: std::env::current_exe().unwrap().display().to_string(),
        args: server_args(),
        env: HashMap::from([(SERVER_ENV.to_string(), "probe".to_string())]),
        restart_policy,
        ..Default::default()
    }
}

#[cfg(unix)]
fn fast_restarts(max_restart_attempts: u32) -> RestartPolicy {
    RestartPolicy {
        initial_delay_ms: 20,
        max_delay_ms: 100,
        backoff_multiplier: 2.0,
        max_restart_attempts,
    }
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-status"))
}

/// The text the managed server answers `tool` with.
async fn call(manager: &McpServerManager, tool: &str) -> String {
    let tools = manager.tools(ctx()).await.unwrap();
    let tool = tools.iter().find(|candidate| candidate.name() == tool).unwrap();
    let output = tool.execute(Arc::new(SimpleToolContext::new("mcp-status")), json!({})).await;
    output.unwrap()["output"].as_str().unwrap().to_string()
}

#[cfg(unix)]
fn transition(from: ServerStatus, to: ServerStatus) -> ServerStatusTransition {
    ServerStatusTransition { id: "probe".to_string(), from, to }
}

/// Receives the next `count` transitions.
#[cfg(unix)]
async fn next_transitions(
    receiver: &mut broadcast::Receiver<ServerStatusTransition>,
    count: usize,
) -> Vec<ServerStatusTransition> {
    let mut seen = Vec::new();
    tokio::time::timeout(DEADLINE, async {
        while seen.len() < count {
            seen.push(receiver.recv().await.unwrap());
        }
    })
    .await
    .unwrap_or_else(|_| panic!("transitions stopped at {seen:?}"));
    seen
}

#[cfg(unix)]
#[tokio::test]
async fn a_subscriber_sees_a_killed_server_crash_and_restart_in_order() {
    use ServerStatus::{Crashed, Restarting, Running, Starting, Stopped};
    let manager = McpServerManager::new(HashMap::from([(
        "probe".to_string(),
        server_config(Some(fast_restarts(3))),
    )]))
    .with_health_check_interval(Duration::from_millis(50));
    let mut transitions = manager.subscribe_status_transitions();

    manager.start_server("probe").await.unwrap();
    let pid = call(&manager, "server_pid").await;
    manager.start_monitoring();
    let killed = std::process::Command::new("kill").args(["-9", &pid]).status().unwrap();
    assert!(killed.success());

    let expected = vec![
        transition(Stopped, Starting),
        transition(Starting, Running),
        transition(Running, Crashed),
        transition(Crashed, Restarting),
        transition(Restarting, Starting),
        transition(Starting, Running),
    ];
    let seen = next_transitions(&mut transitions, expected.len()).await;
    assert_eq!(seen, expected);
    assert_ne!(call(&manager, "server_pid").await, pid, "the restart spawned a new process");

    manager.shutdown().await.unwrap();
    assert_eq!(transitions.recv().await.unwrap(), transition(Running, Stopped));
}

#[cfg(unix)]
#[tokio::test]
async fn a_server_whose_restarts_all_fail_ends_failed_to_start() {
    use ServerStatus::{Crashed, FailedToStart, Restarting, Running, Starting, Stopped};
    // The first start spawns the server; every restart's launch is refused.
    let launches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&launches);
    let manager = McpServerManager::new(HashMap::from([(
        "probe".to_string(),
        server_config(Some(fast_restarts(2))),
    )]))
    .with_health_check_interval(Duration::from_millis(50))
    .with_stdio_launcher(move |config| {
        if counted.fetch_add(1, Ordering::SeqCst) > 0 {
            return Err(AdkError::tool("the sandbox is unavailable"));
        }
        let mut command = tokio::process::Command::new(&config.command);
        command.args(&config.args).envs(&config.env);
        Ok(command)
    });
    let mut transitions = manager.subscribe_status_transitions();

    manager.start_server("probe").await.unwrap();
    let pid = call(&manager, "server_pid").await;
    manager.start_monitoring();
    let killed = std::process::Command::new("kill").args(["-9", &pid]).status().unwrap();
    assert!(killed.success());

    let expected = vec![
        transition(Stopped, Starting),
        transition(Starting, Running),
        transition(Running, Crashed),
        transition(Crashed, Restarting),
        transition(Restarting, Starting),
        transition(Starting, FailedToStart),
        transition(FailedToStart, Restarting),
        transition(Restarting, Starting),
        transition(Starting, FailedToStart),
    ];
    let seen = next_transitions(&mut transitions, expected.len()).await;
    assert_eq!(seen, expected);

    // Two failed restarts exhaust the policy: the monitor makes no further attempt.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(matches!(transitions.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
    assert_eq!(manager.server_status("probe").await.unwrap(), FailedToStart);
    assert_eq!(launches.load(Ordering::SeqCst), 3);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_launchers_command_is_the_one_the_manager_spawns() {
    // The configured command does not exist; only the launcher's command can start.
    let config = McpServerConfig {
        command: "/nonexistent/adk-tool-probe-server".to_string(),
        ..server_config(None)
    };
    let seen_configs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen_configs);
    let manager = McpServerManager::new(HashMap::from([("probe".to_string(), config.clone())]))
        .with_stdio_launcher(move |config| {
            recorded.lock().unwrap().push(config.clone());
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command.args(&config.args).envs(&config.env).env(TAG_ENV, "launched");
            Ok(command)
        });

    manager.start_server("probe").await.unwrap();

    assert_eq!(call(&manager, "launch_tag").await, "launched");
    assert_eq!(*seen_configs.lock().unwrap(), vec![config]);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn without_a_launcher_the_configured_command_is_spawned() {
    let manager =
        McpServerManager::new(HashMap::from([("probe".to_string(), server_config(None))]));

    manager.start_server("probe").await.unwrap();
    assert_eq!(call(&manager, "launch_tag").await, "untagged");
    manager.shutdown().await.unwrap();

    let missing = McpServerConfig {
        command: "/nonexistent/adk-tool-probe-server".to_string(),
        ..server_config(None)
    };
    manager.update_server("probe", missing).await.unwrap();
    let error = manager.start_server("probe").await.unwrap_err();
    assert!(
        error.message.contains("command '/nonexistent/adk-tool-probe-server' not found"),
        "{}",
        error.message
    );
}

/// Calls `launch_tag` until a function response arrives, then answers.
struct CallsLaunchTag;

#[adk_core::async_trait]
impl adk_core::Llm for CallsLaunchTag {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(
        &self,
        request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let answered = request
            .contents
            .iter()
            .flat_map(|content| &content.parts)
            .any(|part| matches!(part, adk_core::Part::FunctionResponse { .. }));
        let part = if answered {
            adk_core::Part::Text { text: "the server was launched".to_string() }
        } else {
            adk_core::Part::FunctionCall {
                name: "launch_tag".to_string(),
                args: json!({}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }
        };
        let response = adk_core::LlmResponse {
            content: Some(adk_core::Content { role: "model".to_string(), parts: vec![part] }),
            turn_complete: true,
            ..Default::default()
        };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

#[tokio::test]
async fn an_agents_tool_call_reaches_the_server_the_launcher_started() {
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    use futures::StreamExt;

    let manager = Arc::new(
        McpServerManager::new(HashMap::from([("probe".to_string(), server_config(None))]))
            .with_stdio_launcher(|config| {
                let mut command = tokio::process::Command::new(&config.command);
                command.args(&config.args).envs(&config.env).env(TAG_ENV, "launched");
                Ok(command)
            }),
    );
    manager.start_server("probe").await.unwrap();
    let agent = adk_agent::LlmAgentBuilder::new("desk")
        .model(Arc::new(CallsLaunchTag))
        .toolset(manager.clone())
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "mcp-status".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = adk_runner::Runner::builder()
        .app_name("mcp-status")
        .agent(Arc::new(agent) as Arc<dyn adk_core::Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();

    let mut stream = runner
        .run_str("user", "session", adk_core::Content::new("user").with_text("Which tag?"))
        .await
        .unwrap();
    let mut responses = Vec::new();
    while let Some(event) = stream.next().await {
        for part in event.unwrap().llm_response.content.iter().flat_map(|content| &content.parts) {
            if let adk_core::Part::FunctionResponse { function_response, .. } = part {
                responses.push(function_response.response.clone());
            }
        }
    }

    assert_eq!(responses, vec![json!({ "output": "launched" })]);
    manager.shutdown().await.unwrap();
}

/// Whether the process `pid` has ended, or ends within `wait`.
#[cfg(windows)]
fn ends_within(pid: u32, wait: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: plain arguments; a null handle means no such process is left.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return true;
    }
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: the handle is open with `PROCESS_SYNCHRONIZE` and closed once.
    unsafe {
        let waited = WaitForSingleObject(process, millis);
        CloseHandle(process);
        waited == WAIT_OBJECT_0
    }
}

/// Ends the process `pid`, should a test leave it running.
#[cfg(windows)]
fn terminate(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: plain arguments; the handle is checked and closed once.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !process.is_null() {
            TerminateProcess(process, 1);
            CloseHandle(process);
        }
    }
}

/// A server's Job Object holds every process it starts, so stopping the server ends them
/// too; without it the descendant runs on after its server (spec 3.2 §10 W1).
#[cfg(windows)]
#[tokio::test]
async fn stopping_a_server_ends_the_processes_it_started() {
    let manager =
        McpServerManager::new(HashMap::from([("probe".to_string(), server_config(None))]));
    manager.start_server("probe").await.unwrap();
    let descendant: u32 = call(&manager, "spawn_descendant").await.parse().unwrap();
    assert!(!ends_within(descendant, Duration::from_millis(500)), "the descendant started");

    manager.stop_server("probe").await.unwrap();

    let ended = ends_within(descendant, Duration::from_secs(10));
    terminate(descendant);
    assert!(ended, "a process the server started outlived the server");
    manager.shutdown().await.unwrap();
}

/// Dropping the manager without stopping its servers ends their trees as well, as the
/// manager's process exiting would.
#[cfg(windows)]
#[tokio::test]
async fn dropping_the_manager_ends_the_processes_its_servers_started() {
    let manager =
        McpServerManager::new(HashMap::from([("probe".to_string(), server_config(None))]));
    manager.start_server("probe").await.unwrap();
    let descendant: u32 = call(&manager, "spawn_descendant").await.parse().unwrap();
    assert!(!ends_within(descendant, Duration::from_millis(500)), "the descendant started");

    drop(manager);

    let ended = ends_within(descendant, Duration::from_secs(10));
    terminate(descendant);
    assert!(ended, "a process a server started outlived the dropped manager");
}
