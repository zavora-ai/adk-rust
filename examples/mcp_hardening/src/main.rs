//! `LlmAgent` + `Runner` + `McpToolset` against scripted MCP servers that page forever,
//! loop their cursors, hang, publish duplicate or unroutable names, collide under the
//! manager's prefixes, annotate their tools, or fail with a 5xx.
//!
//! Every scenario prints PASS or FAIL; the process exits non-zero on any FAIL.

mod servers;
mod upstream;

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Llm, Part, ReadonlyContext, Tool, ToolEffect, Toolset};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::SimpleToolContext;
use adk_tool::mcp::manager::{McpServerConfig, McpServerManager};
use adk_tool::mcp::rmcp::model::ToolAnnotations;
use adk_tool::mcp::{McpDiscoveryLimits, McpHttpClientBuilder, McpToolset, RefreshConfig};
use adk_tool::toolset::PrefixedToolset;
use futures::StreamExt;
use serde_json::json;

use servers::{
    Catalog, CyclicCursor, EndlessCursor, FIXTURE_FLAG, TRACKING_CODE, Versioned, connect, tool,
};
use upstream::ScriptedGemini;

/// How long a scenario may run before it counts as hung.
const WATCHDOG: Duration = Duration::from_secs(15);

/// How long the live model turn, a tool call and an answer, may take.
const LIVE_WATCHDOG: Duration = Duration::from_secs(90);

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

fn pass(detail: impl Into<String>) -> anyhow::Result<Verdict> {
    Ok(Verdict::Pass(detail.into()))
}

fn fail(detail: impl Into<String>) -> anyhow::Result<Verdict> {
    Ok(Verdict::Fail(detail.into()))
}

/// The first line of `text`, at most 200 characters.
fn brief(text: &str) -> String {
    text.lines().next().unwrap_or_default().chars().take(200).collect()
}

fn ctx() -> Arc<dyn ReadonlyContext> {
    Arc::new(SimpleToolContext::new("mcp-hardening"))
}

/// One finished agent turn.
struct Turn {
    error: Option<String>,
    text: String,
}

/// Runs one turn of an `LlmAgent` over `toolsets` through a `Runner`.
async fn run_agent(
    model: Arc<dyn Llm>,
    toolsets: Vec<Arc<dyn Toolset>>,
    strict: bool,
    prompt: &str,
) -> anyhow::Result<Turn> {
    let mut builder = LlmAgentBuilder::new("order_desk")
        .model(model)
        .instruction(
            "You answer questions about orders. Call the lookup_order tool for any order \
             question, then answer in one sentence that includes the tracking code it returns.",
        )
        .strict_toolsets(strict);
    for toolset in toolsets {
        builder = builder.toolset(toolset);
    }
    let agent = builder.build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let (app, user, session) = ("mcp-hardening", "user-1", "session-1");
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: Some(session.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name(app)
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()?;
    let mut stream =
        match runner.run_str(user, session, Content::new("user").with_text(prompt)).await {
            Ok(stream) => stream,
            Err(error) => return Ok(Turn { error: Some(error.to_string()), text: String::new() }),
        };
    let mut turn = Turn { error: None, text: String::new() };
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                if let Some(message) = &event.llm_response.error_message {
                    turn.error.get_or_insert_with(|| message.clone());
                }
                // Streamed deltas are repeated in the final event, so read only that.
                if event.llm_response.partial {
                    continue;
                }
                for part in event.llm_response.content.iter().flat_map(|content| &content.parts) {
                    if let Part::Text { text } = part {
                        turn.text.push_str(text);
                    }
                }
            }
            Err(error) => {
                turn.error = Some(error.to_string());
                break;
            }
        }
    }
    Ok(turn)
}

/// Bounds `future` by the watchdog, reporting a hang as `None`.
async fn bounded<T>(future: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(WATCHDOG, future).await.ok()
}

/// 1. A server that returns a fresh `nextCursor` forever.
async fn endless_cursor() -> anyhow::Result<Verdict> {
    let server = EndlessCursor::default();
    let lists = Arc::clone(&server.lists);
    let toolset = McpToolset::new(connect(server).await?).with_name("endless");
    let gemini = ScriptedGemini::start(None).await?;
    let started = Instant::now();
    let turn = bounded(run_agent(
        Arc::new(gemini.model()?),
        vec![Arc::new(toolset)],
        true,
        "List your tools.",
    ))
    .await;
    let (elapsed, pages) = (started.elapsed(), lists.load(Ordering::SeqCst));
    match turn {
        None => fail(format!("turn still discovering after {elapsed:.1?}; {pages} pages served")),
        Some(turn) => match turn?.error {
            Some(error) if pages <= 64 && gemini.requests() == 0 => pass(format!(
                "turn failed in {elapsed:.1?} after {pages} pages, before any model call: {}",
                brief(&error)
            )),
            Some(error) => {
                fail(format!("{pages} pages, {} model calls: {}", gemini.requests(), brief(&error)))
            }
            None => fail(format!("turn succeeded after {pages} pages")),
        },
    }
}

/// 2. A server whose cursors cycle `a -> b -> a` with a `ttlMs`.
async fn cyclic_cursor() -> anyhow::Result<Verdict> {
    let server = CyclicCursor::default();
    let lists = Arc::clone(&server.lists);
    let toolset = McpToolset::new(connect(server).await?).with_name("cyclic");
    let gemini = ScriptedGemini::start(None).await?;
    let started = Instant::now();
    let turn = bounded(run_agent(
        Arc::new(gemini.model()?),
        vec![Arc::new(toolset)],
        true,
        "List your tools.",
    ))
    .await;
    let (elapsed, requests) = (started.elapsed(), lists.load(Ordering::SeqCst));
    match turn {
        None => fail(format!(
            "turn still discovering after {elapsed:.1?}; the server saw only {requests} tools/list requests"
        )),
        Some(turn) => match turn?.error {
            Some(error) if requests <= 3 => pass(format!(
                "turn failed in {elapsed:.1?} after {requests} tools/list requests: {}",
                brief(&error)
            )),
            Some(error) => fail(format!("{requests} requests: {}", brief(&error))),
            None => fail("turn succeeded on a cyclic cursor"),
        },
    }
}

fn long_name(suffix: &str) -> String {
    format!(
        "{}_{suffix}",
        "inventory_reconciliation_report".repeat(3).chars().take(69).collect::<String>()
    )
}

/// The misbehaving server: a duplicate name, a name with control characters, and two
/// names that only differ after the 64th byte.
fn misbehaving_catalog() -> Catalog {
    Catalog::new(
        "misbehaving",
        vec![
            tool("dup", "First definition."),
            tool("dup", "Second definition."),
            tool("line\nbreak\u{1b}[31m", "Name with control characters."),
            tool(&long_name("a"), "Long name a."),
            tool(&long_name("b"), "Long name b."),
        ],
    )
}

/// Checks the function names an agent declared to the model.
fn declared_problems(names: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let unique: BTreeSet<_> = names.iter().collect();
    if unique.len() != names.len() {
        problems.push(format!("duplicate declarations in {names:?}"));
    }
    for name in names {
        if name.len() > 64 {
            problems.push(format!("{}-byte name declared", name.len()));
        }
        if name.chars().any(char::is_control) {
            problems.push(format!("control characters declared in {name:?}"));
        }
    }
    problems
}

/// 3. A server publishing duplicate and unroutable names beside a healthy server.
async fn duplicate_names_beside_healthy_server() -> anyhow::Result<Verdict> {
    let healthy = Catalog::orders();
    let toolsets: Vec<Arc<dyn Toolset>> = vec![
        Arc::new(McpToolset::new(connect(misbehaving_catalog()).await?).with_name("misbehaving")),
        Arc::new(McpToolset::new(connect(healthy.clone()).await?).with_name("orders")),
    ];
    let gemini =
        ScriptedGemini::start(Some(("lookup_order", json!({ "order_id": "A-1042" })))).await?;
    let Some(turn) =
        bounded(run_agent(Arc::new(gemini.model()?), toolsets, false, "Where is order A-1042?"))
            .await
    else {
        return fail("turn hung");
    };
    let turn = turn?;
    if let Some(error) = turn.error {
        return fail(format!("turn failed: {}", brief(&error)));
    }
    let declared = gemini.first_declared();
    let problems = declared_problems(&declared);
    if !problems.is_empty() {
        return fail(problems.join("; "));
    }
    if healthy.calls_to("lookup_order") != 1 || !turn.text.contains(TRACKING_CODE) {
        return fail(format!(
            "lookup_order calls: {}, answer: {}",
            healthy.calls_to("lookup_order"),
            brief(&turn.text)
        ));
    }
    pass(format!("answered from the healthy server; declared {declared:?}"))
}

/// 4. The same pair of servers with a live Gemini model.
async fn live_gemini_turn() -> anyhow::Result<Verdict> {
    let key = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .ok()
        .filter(|key| !key.trim().is_empty());
    let Some(key) = key else {
        return Ok(Verdict::Skip("GEMINI_API_KEY is not set".to_string()));
    };
    let model_name =
        std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-3.5-flash-lite".to_string());
    for attempt in 1..=2 {
        let healthy = Catalog::orders();
        let toolsets: Vec<Arc<dyn Toolset>> = vec![
            Arc::new(
                McpToolset::new(connect(misbehaving_catalog()).await?).with_name("misbehaving"),
            ),
            Arc::new(McpToolset::new(connect(healthy.clone()).await?).with_name("orders")),
        ];
        let model = Arc::new(GeminiModel::new(key.clone(), &model_name)?);
        let turn = run_agent(
            model,
            toolsets,
            false,
            "Use the lookup_order tool to find order A-1042 and tell me its tracking code.",
        );
        let Ok(turn) = tokio::time::timeout(LIVE_WATCHDOG, turn).await else {
            return fail(format!("turn did not finish within {LIVE_WATCHDOG:?}"));
        };
        let turn = turn?;
        if let Some(error) = turn.error {
            return fail(format!("turn failed: {}", brief(&error)));
        }
        let calls = healthy.calls_to("lookup_order");
        if calls >= 1 && turn.text.contains(TRACKING_CODE) {
            return pass(format!(
                "{model_name} called lookup_order {calls}x: {}",
                brief(&turn.text)
            ));
        }
        if attempt == 2 {
            return fail(format!("lookup_order calls: {calls}, answer: {}", brief(&turn.text)));
        }
    }
    fail("unreachable")
}

fn fixture(mode: &str) -> anyhow::Result<McpServerConfig> {
    Ok(McpServerConfig {
        command: std::env::current_exe()?.display().to_string(),
        args: vec![FIXTURE_FLAG.to_string(), mode.to_string()],
        ..Default::default()
    })
}

async fn start_manager(manager: &McpServerManager) -> anyhow::Result<()> {
    for (id, result) in manager.start_all().await {
        result.map_err(|error| anyhow::anyhow!("server {id} failed to start: {error}"))?;
    }
    Ok(())
}

/// 5. A managed server that never answers `tools/list` beside a healthy one.
async fn manager_hung_server() -> anyhow::Result<Verdict> {
    let manager = Arc::new(
        McpServerManager::new(HashMap::from([
            ("stuck".to_string(), fixture("hung")?),
            ("orders".to_string(), fixture("orders")?),
        ]))
        .with_discovery_limits(McpDiscoveryLimits::default().with_timeout(Duration::from_secs(2))),
    );
    start_manager(&manager).await?;
    let gemini =
        ScriptedGemini::start(Some(("lookup_order", json!({ "order_id": "A-1042" })))).await?;
    let started = Instant::now();
    let turn = bounded(run_agent(
        Arc::new(gemini.model()?),
        vec![Arc::clone(&manager) as Arc<dyn Toolset>],
        false,
        "Where is order A-1042?",
    ))
    .await;
    let elapsed = started.elapsed();
    let _ = tokio::time::timeout(Duration::from_secs(10), manager.shutdown()).await;
    let Some(turn) = turn else {
        return fail(format!("turn still discovering after {elapsed:.1?}"));
    };
    let turn = turn?;
    match turn.error {
        None if turn.text.contains(TRACKING_CODE) => {
            pass(format!("answered from the healthy server in {elapsed:.1?}"))
        }
        None => fail(format!("answer without the tool result: {}", brief(&turn.text))),
        Some(error) => fail(format!("turn failed: {}", brief(&error))),
    }
}

/// 6. Managed servers whose prefixed names collide: `a` + `b__x` and `a__b` + `x`.
async fn manager_prefix_collision() -> anyhow::Result<Verdict> {
    let manager = Arc::new(McpServerManager::new(HashMap::from([
        ("a".to_string(), fixture("a")?),
        ("a__b".to_string(), fixture("a__b")?),
        ("z".to_string(), fixture("z")?),
    ])));
    start_manager(&manager).await?;
    let names: Vec<String> = match bounded(manager.tools(ctx())).await {
        Some(Ok(tools)) => tools.iter().map(|tool| tool.name().to_string()).collect(),
        Some(Err(error)) => return fail(format!("manager discovery failed: {error}")),
        None => return fail("manager discovery hung"),
    };
    let gemini = ScriptedGemini::start(Some(("z__x", json!({})))).await?;
    let turn = bounded(run_agent(
        Arc::new(gemini.model()?),
        vec![Arc::clone(&manager) as Arc<dyn Toolset>],
        false,
        "Run x on server z.",
    ))
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(10), manager.shutdown()).await;
    let mut sorted = names.clone();
    sorted.sort();
    let problems = declared_problems(&names);
    if !problems.is_empty() {
        return fail(format!("manager tools {sorted:?}: {}", problems.join("; ")));
    }
    let Some(turn) = turn else {
        return fail("turn hung");
    };
    let turn = turn?;
    match turn.error {
        None if turn.text.contains("answered by fixture z") => {
            pass(format!("manager tools {sorted:?}; z__x routed to server z"))
        }
        None => fail(format!("z__x did not reach server z: {}", brief(&turn.text))),
        Some(error) => fail(format!("turn failed: {}", brief(&error))),
    }
}

/// 7. Annotations become the tool's effect, survive `PrefixedToolset`, and are readable.
async fn annotations() -> anyhow::Result<Verdict> {
    let catalog = Catalog::new(
        "home",
        vec![
            tool("set_thermostat", "Sets the target temperature.").annotate(
                ToolAnnotations::with_title("Set thermostat")
                    .idempotent(true)
                    .destructive(false)
                    .open_world(false),
            ),
            tool("read_temperature", "Reads the temperature.")
                .annotate(ToolAnnotations::new().read_only(true)),
            tool("delete_logs", "Deletes the logs.")
                .annotate(ToolAnnotations::new().destructive(true)),
        ],
    );
    let toolset = McpToolset::new(connect(catalog).await?).with_name("home");
    let direct: HashMap<String, Arc<dyn Tool>> = toolset
        .tools(ctx())
        .await?
        .into_iter()
        .map(|tool| (tool.name().to_string(), tool))
        .collect();
    let prefixed: HashMap<String, Arc<dyn Tool>> = PrefixedToolset::new(Arc::new(toolset), "home")
        .tools(ctx())
        .await?
        .into_iter()
        .map(|tool| (tool.name().to_string(), tool))
        .collect();
    let effects = |tools: &HashMap<String, Arc<dyn Tool>>, prefix: &str| {
        ["set_thermostat", "read_temperature", "delete_logs"]
            .map(|name| tools.get(&format!("{prefix}{name}")).map(|tool| tool.effect()))
    };
    let expected =
        [Some(ToolEffect::Idempotent), Some(ToolEffect::ReadOnly), Some(ToolEffect::NonIdempotent)];
    let mut problems = Vec::new();
    for (label, tools, prefix) in [("direct", &direct, ""), ("prefixed", &prefixed, "home_")] {
        let found = effects(tools, prefix);
        if found != expected {
            problems.push(format!("{label} effects {found:?}, expected {expected:?}"));
        }
        let reader = tools.get(&format!("{prefix}read_temperature"));
        if !reader.is_some_and(|tool| tool.is_read_only() && tool.is_concurrency_safe()) {
            problems
                .push(format!("{label} read_temperature is not read-only and concurrency-safe"));
        }
        // The server's hints, typed again through the rmcp re-export.
        let hints = tools
            .get(&format!("{prefix}set_thermostat"))
            .and_then(|tool| tool.annotations().cloned())
            .and_then(|hints| serde_json::from_value::<ToolAnnotations>(hints).ok());
        let expected_hints = ToolAnnotations::with_title("Set thermostat")
            .idempotent(true)
            .destructive(false)
            .open_world(false);
        if hints.as_ref() != Some(&expected_hints) {
            problems.push(format!("{label} set_thermostat annotations {hints:?}"));
        }
    }
    if problems.is_empty() {
        pass(
            "idempotentHint -> Idempotent, readOnlyHint -> ReadOnly; title, destructiveHint and \
             openWorldHint readable through Tool::annotations, also behind PrefixedToolset",
        )
    } else {
        fail(problems.join("; "))
    }
}

/// 8. A Streamable HTTP server answers one `tools/list` with HTTP 500 and the body
/// "upstream geoffrey is restarting".
async fn http_5xx_geoffrey() -> anyhow::Result<Verdict> {
    let (url, counters) = upstream::flaky_http_mcp().await?;
    let toolset = McpHttpClientBuilder::new(&url)
        .timeout(Duration::from_secs(5))
        .connect()
        .await?
        .with_refresh_config(RefreshConfig::default().with_retry_delay_ms(0).without_logging())
        .with_tool_list_cache_ttl(Duration::ZERO);
    let first = bounded(toolset.tools(ctx())).await;
    let second = bounded(toolset.tools(ctx())).await;
    let initializes = counters.initializes.load(Ordering::SeqCst);
    let sessions =
        counters.list_sessions.lock().map(|sessions| sessions.clone()).unwrap_or_default();
    let names = |result: &Option<adk_core::Result<Vec<Arc<dyn Tool>>>>| match result {
        Some(Ok(tools)) => {
            format!("{:?}", tools.iter().map(|tool| tool.name()).collect::<Vec<_>>())
        }
        Some(Err(error)) => format!("error: {}", brief(&error.to_string())),
        None => "hung".to_string(),
    };
    let detail = format!(
        "initialize x{initializes}, tools/list sessions {sessions:?}, first {}, second {}",
        names(&first),
        names(&second)
    );
    let first_failed = matches!(&first, Some(Err(_)));
    let second_ok = matches!(&second, Some(Ok(tools)) if tools.len() == 1);
    if initializes == 1
        && first_failed
        && second_ok
        && sessions.iter().all(|session| session == "s1")
    {
        pass(format!("no reconnect; {detail}"))
    } else {
        fail(detail)
    }
}

/// 9. Invalidating the tool list mid-fetch, and with a server `ttlMs`, reaches the server.
async fn invalidation() -> anyhow::Result<Verdict> {
    let mut problems = Vec::new();

    let server = Versioned { hold_first: true, ..Versioned::default() };
    server.version.store(1, Ordering::SeqCst);
    let (version, lists, release) =
        (Arc::clone(&server.version), Arc::clone(&server.lists), Arc::clone(&server.release));
    let toolset = McpToolset::new(connect(server).await?);
    let in_flight = tokio::spawn({
        let toolset = toolset.clone();
        async move { toolset.tools(ctx()).await }
    });
    while lists.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    version.store(2, Ordering::SeqCst);
    toolset.invalidate_tool_list_cache().await;
    release.notify_one();
    in_flight.await??;
    let next: Vec<String> =
        toolset.tools(ctx()).await?.iter().map(|tool| tool.name().to_string()).collect();
    if next != ["v2"] {
        problems.push(format!("after invalidating mid-fetch, tools() returned {next:?}"));
    }

    let server = Versioned { ttl: true, ..Versioned::default() };
    server.version.store(1, Ordering::SeqCst);
    let (version, lists) = (Arc::clone(&server.version), Arc::clone(&server.lists));
    let toolset = McpToolset::new(connect(server).await?);
    toolset.tools(ctx()).await?;
    version.store(2, Ordering::SeqCst);
    toolset.invalidate_tool_list_cache().await;
    let next: Vec<String> =
        toolset.tools(ctx()).await?.iter().map(|tool| tool.name().to_string()).collect();
    if next != ["v2"] {
        problems.push(format!(
            "with a server ttlMs, invalidation returned {next:?} after {} tools/list requests",
            lists.load(Ordering::SeqCst)
        ));
    }

    if problems.is_empty() {
        pass("both invalidations reached the server")
    } else {
        fail(problems.join("; "))
    }
}

/// 10. `call_tool_value` honours the toolset's static tool filter.
async fn call_tool_value_filter() -> anyhow::Result<Verdict> {
    let catalog =
        Catalog::new("filtered", vec![tool("public", "Public."), tool("secret", "Secret.")]);
    let toolset = McpToolset::new(connect(catalog.clone()).await?).with_tools(&["public"]);
    let result = toolset.call_tool_value("secret", Default::default()).await;
    match result {
        Err(error) if catalog.calls_to("secret") == 0 => {
            pass(format!("refused without a server call: {}", brief(&error.to_string())))
        }
        Err(error) => fail(format!("refused after calling the server: {error}")),
        Ok(value) => fail(format!("filtered tool ran: {value}")),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some(FIXTURE_FLAG) {
        return servers::serve_stdio(&args.next().unwrap_or_default()).await;
    }
    // The repository's `.env`, else one in the working directory or a parent.
    if dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env")).is_err() {
        let _ = dotenvy::dotenv();
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .init();

    println!("MCP hardening: LlmAgent + McpToolset against scripted MCP servers\n");
    let scenarios: Vec<(&str, std::pin::Pin<Box<dyn Future<Output = anyhow::Result<Verdict>>>>)> = vec![
        ("endless_cursor", Box::pin(endless_cursor())),
        ("cyclic_cursor", Box::pin(cyclic_cursor())),
        (
            "duplicate_names_beside_healthy_server",
            Box::pin(duplicate_names_beside_healthy_server()),
        ),
        ("live_gemini_turn", Box::pin(live_gemini_turn())),
        ("manager_hung_server", Box::pin(manager_hung_server())),
        ("manager_prefix_collision", Box::pin(manager_prefix_collision())),
        ("annotations", Box::pin(annotations())),
        ("http_5xx_geoffrey", Box::pin(http_5xx_geoffrey())),
        ("invalidation", Box::pin(invalidation())),
        ("call_tool_value_filter", Box::pin(call_tool_value_filter())),
    ];
    let mut failures = 0;
    for (name, scenario) in scenarios {
        let limit = if name == "live_gemini_turn" { LIVE_WATCHDOG * 2 } else { WATCHDOG * 2 };
        let verdict = match tokio::time::timeout(limit, scenario).await {
            Ok(Ok(verdict)) => verdict,
            Ok(Err(error)) => Verdict::Fail(format!("setup failed: {error}")),
            Err(_) => Verdict::Fail(format!("no result within {limit:?}")),
        };
        let (label, detail) = match verdict {
            Verdict::Pass(detail) => ("PASS", detail),
            Verdict::Fail(detail) => {
                failures += 1;
                ("FAIL", detail)
            }
            Verdict::Skip(detail) => ("SKIP", detail),
        };
        println!("{label}  {name}: {detail}");
    }
    println!();
    if failures > 0 {
        println!("{failures} scenario(s) failed");
        std::process::exit(1);
    }
    println!("all scenarios passed");
    Ok(())
}
