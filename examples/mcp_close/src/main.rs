//! `Runner::close` on an `LlmAgent` whose tools come from MCP servers: a stdio server
//! process exits, a Streamable HTTP session ends, toolsets composed with
//! `PrefixedToolset`, `MergedToolset` and `FilteredToolset` close their members, a closed
//! toolset fails later runs instead of reconnecting, and closing is bounded and idempotent.
//!
//! Every scenario prints PASS, FAIL or SKIP; the process exits non-zero on any FAIL.

mod servers;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Llm, Part, ReadonlyContext, Toolset, async_trait};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::manager::{McpServerConfig, McpServerManager, ServerStatus};
use adk_tool::mcp::{ConnectionFactory, McpHttpClientBuilder, McpToolset};
use adk_tool::toolset::{FilteredToolset, MergedToolset, PrefixedToolset, string_predicate};
use futures::StreamExt;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::json;

use servers::{SERVER_FLAG, ScriptedGemini, TRACKING_CODE};

const APP: &str = "mcp-close";
const USER: &str = "user-1";
const SESSION: &str = "session-1";
const LIVE_MODEL: &str = "gemini-3.5-flash-lite";

/// How long a scenario may run before it counts as hung.
const WATCHDOG: Duration = Duration::from_secs(60);

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Passes when every check holds; the evidence lists each check.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let evidence = checks
        .iter()
        .map(|(ok, text)| format!("[{}] {text}", if *ok { "ok" } else { "FAILED" }))
        .collect::<Vec<_>>()
        .join("\n      ");
    if checks.iter().all(|(ok, _)| *ok) { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

/// The first line of `text`, at most 160 characters.
fn brief(text: &str) -> String {
    text.lines().next().unwrap_or_default().chars().take(160).collect()
}

/// Whether process `pid` exists. An exited child that nobody reaped still exists.
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Waits up to `limit` for process `pid` to disappear; returns how long it took.
async fn exits_within(pid: u32, limit: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if !process_exists(pid) {
            return Some(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// Starts this binary as a stdio MCP server and completes the handshake with it.
async fn connect_stdio(mode: &str) -> anyhow::Result<(RunningService<RoleClient, ()>, u32)> {
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command.args([SERVER_FLAG, mode]);
    let transport = TokioChildProcess::new(command)?;
    let pid = transport.id().ok_or_else(|| anyhow::anyhow!("the server process has no PID"))?;
    Ok((().serve(transport).await?, pid))
}

/// An `McpToolset` over a fresh stdio server, and the server's PID.
async fn stdio_toolset(name: &str, mode: &str) -> anyhow::Result<(McpToolset, u32)> {
    let (client, pid) = connect_stdio(mode).await?;
    Ok((McpToolset::new(client).with_name(name), pid))
}

/// Reconnects by starting a new stdio server, and counts how often it did.
struct StdioFactory {
    spawned: Arc<AtomicUsize>,
}

#[async_trait]
impl ConnectionFactory<()> for StdioFactory {
    async fn create_connection(&self) -> Result<RunningService<RoleClient, ()>, String> {
        self.spawned.fetch_add(1, Ordering::SeqCst);
        connect_stdio("orders").await.map(|(client, _)| client).map_err(|error| error.to_string())
    }
}

/// One finished run.
#[derive(Default)]
struct Turn {
    error: Option<String>,
    text: String,
    /// Names of the tools whose responses the run recorded.
    tool_responses: Vec<String>,
}

impl Turn {
    fn called(&self, tool: &str) -> bool {
        self.tool_responses.iter().any(|name| name.ends_with(tool))
    }
}

/// A runner over an `LlmAgent` with `toolsets` that fails a turn whose toolsets cannot
/// resolve, so a closed toolset shows up as the run's error.
async fn runner(model: Arc<dyn Llm>, toolsets: Vec<Arc<dyn Toolset>>) -> anyhow::Result<Runner> {
    let mut builder = LlmAgentBuilder::new("order_desk")
        .model(model)
        .instruction(
            "You answer questions about orders. Call the lookup_order tool for any order \
             question, then answer in one sentence that includes the tracking code it returns.",
        )
        .strict_toolsets(true);
    for toolset in toolsets {
        builder = builder.toolset(toolset);
    }
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: Some(SESSION.to_string()),
            state: HashMap::new(),
        })
        .await?;
    Ok(Runner::builder()
        .app_name(APP)
        .agent(Arc::new(builder.build()?) as Arc<dyn Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?)
}

async fn run(runner: &Runner, prompt: &str) -> anyhow::Result<Turn> {
    let mut stream =
        match runner.run_str(USER, SESSION, Content::new("user").with_text(prompt)).await {
            Ok(stream) => stream,
            Err(error) => return Ok(Turn { error: Some(error.to_string()), ..Turn::default() }),
        };
    let mut turn = Turn::default();
    while let Some(item) = stream.next().await {
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                turn.error = Some(error.to_string());
                break;
            }
        };
        if let Some(message) = &event.llm_response.error_message {
            turn.error.get_or_insert_with(|| message.clone());
        }
        if event.llm_response.partial {
            continue;
        }
        for part in event.llm_response.content.iter().flat_map(|content| &content.parts) {
            match part {
                Part::Text { text } => turn.text.push_str(text),
                Part::FunctionResponse { function_response, .. } => {
                    turn.tool_responses.push(function_response.name.clone());
                }
                _ => {}
            }
        }
    }
    Ok(turn)
}

fn ran_ok(turn: &Turn, what: &str) -> (bool, String) {
    match &turn.error {
        None => (turn.called("lookup_order"), format!("{what} called lookup_order")),
        Some(error) => (false, format!("{what} failed: {}", brief(error))),
    }
}

fn refused_as_closed(turn: &Turn, model_calls_before: usize, model_calls_after: usize) -> (bool, String) {
    match &turn.error {
        Some(error) => (
            error.contains("closed") && model_calls_after == model_calls_before,
            format!(
                "a run after close fails before any model call ({} new model calls): {}",
                model_calls_after - model_calls_before,
                brief(error)
            ),
        ),
        None => (
            false,
            format!(
                "a run after close succeeded ({} new model calls); the toolset still served tools",
                model_calls_after - model_calls_before
            ),
        ),
    }
}

fn exited(pid: u32, label: &str) -> (bool, String) {
    let alive = process_exists(pid);
    (
        !alive,
        format!(
            "{label} (PID {pid}) {} when Runner::close returns",
            if alive { "is still running" } else { "has exited" }
        ),
    )
}

/// 1. `Runner::close` ends a stdio MCP server the agent used.
async fn runner_close_stops_stdio_server() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let (toolset, pid) = stdio_toolset("orders", "orders").await?;
    let runner = runner(Arc::new(gemini.model()?), vec![Arc::new(toolset)]).await?;
    let turn = run(&runner, "Where is order A-1042?").await?;
    let started = Instant::now();
    runner.close().await;
    let closing = started.elapsed();
    Ok(verdict(vec![
        ran_ok(&turn, "the run"),
        (turn.text.contains(TRACKING_CODE), format!("the answer carries {TRACKING_CODE}")),
        exited(pid, "the stdio server"),
        (closing < Duration::from_secs(5), format!("Runner::close took {closing:.1?}")),
    ]))
}

/// 2. The same on live Gemini.
async fn live_runner_close_stops_stdio_server() -> anyhow::Result<Verdict> {
    let Some(key) = google_key() else {
        return Ok(Verdict::Skip("GOOGLE_API_KEY is not set".into()));
    };
    let (toolset, pid) = stdio_toolset("orders", "orders").await?;
    let model = Arc::new(GeminiModel::new(&key, LIVE_MODEL)?);
    let runner = runner(model, vec![Arc::new(toolset)]).await?;
    let turn = run(&runner, "Where is order A-1042?").await?;
    let started = Instant::now();
    runner.close().await;
    let closing = started.elapsed();
    Ok(verdict(vec![
        ran_ok(&turn, LIVE_MODEL),
        (
            true,
            format!(
                "the answer {} the tracking code: {}",
                if turn.text.contains(TRACKING_CODE) { "carries" } else { "omits" },
                brief(&turn.text)
            ),
        ),
        exited(pid, "the stdio server"),
        (closing < Duration::from_secs(5), format!("Runner::close took {closing:.1?}")),
    ]))
}

/// 3. A closed toolset with a connection factory fails later runs instead of starting a
///    new server, and a second `Runner::close` is harmless.
async fn closed_toolset_does_not_reconnect() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let spawned = Arc::new(AtomicUsize::new(0));
    let (toolset, pid) = stdio_toolset("orders", "orders").await?;
    let toolset = toolset
        .with_connection_factory(Arc::new(StdioFactory { spawned: Arc::clone(&spawned) }));
    let runner = runner(Arc::new(gemini.model()?), vec![Arc::new(toolset)]).await?;
    let first = run(&runner, "Where is order A-1042?").await?;
    runner.close().await;
    let stopped = exited(pid, "the stdio server");
    let before = gemini.requests();
    let second = run(&runner, "And where is order A-1043?").await?;
    let after = gemini.requests();
    let started = Instant::now();
    runner.close().await;
    let again = started.elapsed();
    let spawned = spawned.load(Ordering::SeqCst);
    Ok(verdict(vec![
        ran_ok(&first, "the first run"),
        stopped,
        refused_as_closed(&second, before, after),
        (spawned == 0, format!("the connection factory started {spawned} new servers")),
        (again < Duration::from_secs(1), format!("a second Runner::close took {again:.1?}")),
    ]))
}

/// 4. `Runner::close` reaches MCP toolsets inside `PrefixedToolset`, `MergedToolset` and
///    `FilteredToolset`.
async fn composed_toolsets_close_members() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let (orders, orders_pid) = stdio_toolset("orders", "orders").await?;
    let (archive, archive_pid) = stdio_toolset("archive", "orders").await?;
    let filtered = FilteredToolset::new(
        Arc::new(archive),
        string_predicate(vec!["server_pid".to_string()]),
    );
    let merged = MergedToolset::new("shop", vec![Arc::new(orders), Arc::new(filtered)]);
    let prefixed = PrefixedToolset::new(Arc::new(merged), "shop");
    let runner = runner(Arc::new(gemini.model()?), vec![Arc::new(prefixed)]).await?;
    let turn = run(&runner, "Where is order A-1042?").await?;
    runner.close().await;
    Ok(verdict(vec![
        ran_ok(&turn, "the run, through shop_lookup_order,"),
        exited(orders_pid, "the merged server"),
        exited(archive_pid, "the filtered server"),
    ]))
}

/// 5. A server that ignores the end of its input cannot stall `Runner::close` past the
///    toolset's close timeout, and it is still stopped.
async fn close_is_bounded() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let (toolset, pid) = stdio_toolset("stubborn", "stubborn").await?;
    #[cfg(not(feature = "main-baseline"))]
    let toolset = toolset.with_close_timeout(Duration::from_millis(500));
    let runner = runner(Arc::new(gemini.model()?), vec![Arc::new(toolset)]).await?;
    let started = Instant::now();
    runner.close().await;
    let first = started.elapsed();
    let started = Instant::now();
    runner.close().await;
    let second = started.elapsed();
    let gone = exits_within(pid, Duration::from_secs(6)).await;
    Ok(verdict(vec![
        (first < Duration::from_millis(1500), format!("Runner::close took {first:.1?}")),
        (second < Duration::from_millis(1500), format!("a second Runner::close took {second:.1?}")),
        match gone {
            Some(after) => (true, format!("the server (PID {pid}) was killed {after:.1?} later")),
            None => (false, format!("the server (PID {pid}) is still running 6 s later")),
        },
    ]))
}

/// 6. `Runner::close` ends a Streamable HTTP MCP session with `DELETE`, and a later run
///    does not open a new one.
async fn http_session_ends() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let (url, log) = servers::http_mcp().await?;
    let toolset = McpHttpClientBuilder::new(url).connect().await?.with_name("orders_http");
    let runner = runner(Arc::new(gemini.model()?), vec![Arc::new(toolset)]).await?;
    let first = run(&runner, "Where is order A-1042?").await?;
    runner.close().await;
    let deleted = log.deleted();
    let before = gemini.requests();
    let second = run(&runner, "And where is order A-1043?").await?;
    let after = gemini.requests();
    Ok(verdict(vec![
        ran_ok(&first, "the first run"),
        (
            deleted == ["session-1"],
            format!("the server saw DELETE for {deleted:?} when Runner::close returned"),
        ),
        refused_as_closed(&second, before, after),
        (
            log.initializes() == 1,
            format!("the server saw {} initialize requests", log.initializes()),
        ),
    ]))
}

/// The PID the managed server `id` reports through its `server_pid` tool.
async fn managed_pid(manager: &McpServerManager) -> anyhow::Result<u32> {
    let ctx: Arc<dyn ReadonlyContext> = Arc::new(SimpleToolContext::new(APP));
    let tools = manager.tools(ctx).await?;
    let tool = tools
        .iter()
        .find(|tool| tool.name() == "server_pid")
        .ok_or_else(|| anyhow::anyhow!("the managed server published no server_pid tool"))?;
    let output = tool.execute(Arc::new(SimpleToolContext::new(APP)), json!({})).await?;
    Ok(output["output"].as_str().unwrap_or_default().trim().parse()?)
}

/// 7. A shared `McpServerManager` keeps its servers through `Runner::close`; its
///    `shutdown` stops them.
async fn manager_servers_outlive_runner_close() -> anyhow::Result<Verdict> {
    let gemini = ScriptedGemini::start().await?;
    let config = McpServerConfig {
        command: std::env::current_exe()?.display().to_string(),
        args: vec![SERVER_FLAG.to_string(), "orders".to_string()],
        ..Default::default()
    };
    let manager = Arc::new(
        McpServerManager::new(HashMap::from([("orders".to_string(), config)]))
            .with_grace_period(Duration::from_secs(3)),
    );
    manager.start_server("orders").await?;
    let pid = managed_pid(&manager).await?;
    let runner =
        runner(Arc::new(gemini.model()?), vec![Arc::clone(&manager) as Arc<dyn Toolset>]).await?;
    let turn = run(&runner, "Where is order A-1042?").await?;
    runner.close().await;
    let status = manager.server_status("orders").await?;
    let alive_after_close = process_exists(pid);
    manager.shutdown().await?;
    let alive_after_shutdown = process_exists(pid);
    Ok(verdict(vec![
        ran_ok(&turn, "the run"),
        (
            status == ServerStatus::Running && alive_after_close,
            format!(
                "after Runner::close the server is {status:?} and its process {}",
                if alive_after_close { "runs" } else { "has exited" }
            ),
        ),
        (
            !alive_after_shutdown,
            format!(
                "the server process {} when McpServerManager::shutdown returns",
                if alive_after_shutdown { "is still running" } else { "has exited" }
            ),
        ),
    ]))
}

fn google_key() -> Option<String> {
    ["GOOGLE_API_KEY", "GEMINI_API_KEY"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

type Scenario = fn() -> Pin<Box<dyn Future<Output = anyhow::Result<Verdict>>>>;

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        vec![$((stringify!($name), (|| Box::pin($name()) as Pin<Box<dyn Future<Output = anyhow::Result<Verdict>>>>) as Scenario)),*]
    };
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some(SERVER_FLAG) {
        return match servers::serve_stdio(&args.next().unwrap_or_default()).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    // The file named by ADK_ENV_FILE, else the repository's `.env`; either overrides the
    // process environment.
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    let _ = dotenvy::from_path_override(env_file);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .init();

    let all: Vec<(&str, Scenario)> = scenarios![
        runner_close_stops_stdio_server,
        live_runner_close_stops_stdio_server,
        closed_toolset_does_not_reconnect,
        composed_toolsets_close_members,
        close_is_bounded,
        http_session_ends,
        manager_servers_outlive_runner_close,
    ];
    let args: Vec<String> = std::env::args().skip(1).collect();
    let selected: Option<Vec<String>> = args
        .iter()
        .position(|arg| arg == "--scenario")
        .and_then(|index| args.get(index + 1))
        .map(|list| list.split(',').map(str::to_string).collect());
    let skip_live = args.iter().any(|arg| arg == "--no-live");

    println!("Runner::close on LlmAgent + MCP toolsets\n");
    let mut failed = 0;
    let mut rows = Vec::new();
    for (name, scenario) in all {
        if selected.as_ref().is_some_and(|names| !names.iter().any(|wanted| wanted == name)) {
            continue;
        }
        if skip_live && name.starts_with("live_") {
            continue;
        }
        let (status, evidence) = match tokio::time::timeout(WATCHDOG, scenario()).await {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failed += 1;
                ("FAIL", evidence)
            }
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Err(error)) => {
                failed += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failed += 1;
                ("FAIL", format!("scenario still running after {WATCHDOG:?}"))
            }
        };
        println!("{status} {name}\n      {evidence}");
        rows.push((name, status));
    }
    println!("\n| Scenario | Result |\n|---|---|");
    for (name, status) in rows {
        println!("| `{name}` | {status} |");
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
