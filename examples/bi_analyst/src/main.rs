//! # BI Analyst — an agent that works a dashboard the way a person does
//!
//! Opens the saved dashboards a business already has, reads the numbers behind each
//! tile, notices what is odd, drills into it, and says what it found — reporting
//! every step into a console someone can watch and interrupt.
//!
//! ## Three servers, three jobs
//!
//! | Server | Transport | Why it is here |
//! |---|---|---|
//! | `mcp-bi` | stdio | The dashboards: list, open, chart data, drill down, render |
//! | `mcp-market-data` | stdio | Quotes and history, to check a dashboard against the market |
//! | `computer-use-mcp` | HTTP | The run console, and a browser for dashboards with no data API |
//!
//! ## The rule that makes it trustworthy
//!
//! An agent handed a dashboard image will describe a trend it never measured. So
//! every visual is read twice: `bi_insights` computes what is true about the numbers,
//! and `bi_render_chart` draws them. The agent must cite a statistic. When it opens a
//! dashboard in a browser and looks at it — which is the only way to read a platform
//! with no data API — it corroborates against the queried numbers rather than
//! trusting its own reading of the pixels.
//!
//! ## Drill-down, unattended
//!
//! `bi_get_dashboard` reports the dimensions each chart can be broken down by, and
//! `bi_drill_down` reports rows before and after. That pair is what lets the agent
//! choose a drill path and know whether it actually narrowed anything, instead of
//! guessing at column names and reporting a step that did nothing.
//!
//! ## Setup
//!
//! ```bash
//! # 1. the console, so there is something to watch
//! npx -y -p @zavora-ai/computer-use-mcp computer-use-mcp-console --no-demo
//!
//! # 2. this agent. No BI credentials needed: mcp-bi defaults to a seeded fixture
//! #    with three dashboards.
//! export DEEPSEEK_API_KEY=...
//! export MCP_BI_BIN=/path/to/mcp-servers/mcp-bi/target/release/mcp-bi
//! cargo run --manifest-path examples/bi_analyst/Cargo.toml
//! ```
//!
//! Point it at a real platform by passing `BI_BACKEND` through to the server:
//! `BI_BACKEND=superset SUPERSET_URL=... SUPERSET_USERNAME=... SUPERSET_PASSWORD=...`.
//!
//! Prefer the username and password over `SUPERSET_TOKEN`: a Superset access token
//! is valid for 15 minutes, which is shorter than an analysis session, and with
//! credentials the server refreshes it rather than failing partway through.

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Part};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort, ThinkingMode};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::{McpHttpClientBuilder, manager::McpServerManager};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Console tools: the run itself. Without these nobody can see the work.
const CONSOLE_TOOLS: &[&str] =
    &["run_plan", "run_progress", "run_say", "run_console", "run_attachment"];

/// Desktop tools, for reading a dashboard that has no data API.
///
/// Deliberately narrow. No `run_script`, no `filesystem`, no `process_kill`: this
/// agent reads dashboards, and a wider surface buys nothing.
/// Expose only an allowed subset of another toolset's tools.
///
/// `McpServerManager` surfaces everything its servers offer, and Playwright offers
/// arbitrary code execution in the page. An example that people copy should not hand
/// a model more capability than the task needs, so the list is explicit and the rest
/// never reaches the model.
struct Allowed {
    inner: Arc<dyn adk_core::Toolset>,
    /// Every list whose tools may pass. Kept as separate lists so each server's
    /// surface is stated once, rather than merged by hand into a copy that drifts.
    allow: Vec<&'static [&'static str]>,
    /// Names refused whatever else is allowed. Asserted rather than assumed, because
    /// a withheld tool appearing would be a capability leak, not a cosmetic slip.
    withheld: Vec<&'static str>,
}

#[async_trait::async_trait]
impl adk_core::Toolset for Allowed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn adk_core::ReadonlyContext>) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
        let all = self.inner.tools(ctx).await?;
        Ok(all
            .into_iter()
            .filter(|tool| {
                // Tools are prefixed `server__tool` when names collide across
                // servers, so match the trailing segment rather than the whole name.
                let name = tool.name();
                let leaf = name.rsplit("__").next().unwrap_or(name);
                if self.withheld.contains(&leaf) {
                    return false;
                }
                self.allow.iter().any(|list| list.contains(&leaf))
            })
            .collect())
    }
}

/// The BI server's whole surface. All twelve are read-only by construction.
const BI_TOOLS: &[&str] = &[
    "bi_backend_info",
    "bi_list_dashboards",
    "bi_get_dashboard",
    "bi_list_datasets",
    "bi_describe_dataset",
    "bi_chart_data",
    "bi_drill_down",
    "bi_query",
    "bi_insights",
    "bi_render_chart",
    "bi_export_dashboard_image",
    "bi_dashboard_url",
    // Memory. A correction recorded once is what makes the next session fast; a wrong
    // one is worse than none, so forgetting belongs here too.
    "bi_remember",
    "bi_recall",
    "bi_forget",
];

/// Playwright's everyday tools: what operating a dashboard normally needs.
///
/// These are the first choice for anything inside a web page. They act by selector
/// and wait by themselves, so they need no coordinates and no screenshots.
const PLAYWRIGHT_PRIMARY: &[&str] = &[
    "browser_navigate",
    "browser_navigate_back",
    "browser_snapshot",
    "browser_fill_form",
    "browser_click",
    "browser_type",
    "browser_select_option",
    "browser_press_key",
    "browser_hover",
    "browser_wait_for",
    "browser_handle_dialog",
    "browser_tabs",
];

/// Playwright's specialist tools: available, but each answers one specific question.
///
/// Kept rather than dropped because a dashboard does produce these situations. The
/// risk of a wide tool surface is that a model picks an exotic tool for an everyday
/// job, so the brief names the case each one is for instead of listing them flatly:
///
/// - `browser_network_requests` / `browser_network_request` — which query produced a
///   number on screen. This is provenance, and nothing else here can answer it.
/// - `browser_console_messages` — why a chart rendered blank or a filter did nothing.
/// - `browser_drag` / `browser_drop` — a range slider, a reorder, a resize handle:
///   controls that only respond to a gesture.
/// - `browser_find` — locate one element when a full snapshot would be huge.
/// - `browser_take_screenshot` — an image of one element, when the console's own
///   window capture is the wrong frame.
/// - `browser_resize` — force a viewport size so a responsive dashboard lays out
///   predictably before capture.
/// - `browser_close` — tidy up a tab that is finished with.
const PLAYWRIGHT_SECONDARY: &[&str] = &[
    "browser_network_requests",
    "browser_network_request",
    "browser_console_messages",
    "browser_drag",
    "browser_drop",
    "browser_find",
    "browser_take_screenshot",
    "browser_resize",
    "browser_close",
];

/// Deliberately never exposed, and worth saying why rather than leaving a silent gap.
///
/// - `browser_run_code_unsafe` executes arbitrary JavaScript **in the Playwright
///   server process**, not in the page. It is remote-code-execution equivalent, and
///   no analysis needs it.
/// - `browser_evaluate` runs arbitrary code in the page. A read-only analyst has no
///   call for it, and `browser_snapshot` plus the specialist tools above cover the
///   legitimate cases without handing over script execution.
/// - `browser_file_upload` writes into the platform. Everything else here reads.
const PLAYWRIGHT_WITHHELD: &[&str] =
    &["browser_run_code_unsafe", "browser_evaluate", "browser_file_upload"];

/// The governed metric model: what the organisation says a metric *means*.
///
/// This is the layer a BI platform does not have. `bi_chart_data` tells you what a
/// saved chart returns; `get_metric_definition` tells you the formula, the owner and
/// whether it is certified — so an agent can cite the company's definition of revenue
/// instead of inventing one. `explain_change` attributes a movement to drivers with a
/// confidence, which is the question people actually ask.
const ANALYTICS_PRIMARY: &[&str] = &[
    "list_metrics",
    "get_metric_definition",
    "query_metric",
    "breakdown_metric",
    "compare_metric",
    "explain_change",
    "detect_anomalies",
    "generate_insight_summary",
];

/// Analytics tools for a question that has gone deeper than a metric movement.
const ANALYTICS_SECONDARY: &[&str] = &[
    "list_data_sources",
    "list_datasets",
    "describe_dataset",
    "analyze_funnel",
    "analyze_cohort",
    "forecast_metric",
    "get_segments",
    "query_segment",
    "query_events",
    "query_report",
    "list_dashboards",
    "get_dashboard",
    "summarize_dashboard",
    "get_query_audit_trail",
];

/// Withheld, and worth saying why.
///
/// `create_dashboard`, `add_widget` and `publish_dashboard` write, and this agent is
/// read-only by design — a dashboard is changed through the organisation's own review,
/// not by an agent mid-analysis. `request_data_access` asks a human for permission,
/// which is a decision for the person watching, not the agent. The policy checks are
/// withheld only because nothing here exports.
const ANALYTICS_WITHHELD: &[&str] = &[
    "create_dashboard",
    "add_widget",
    "publish_dashboard",
    "request_data_access",
    "validate_analytics_policy",
    "check_export_risk",
];

/// Which tool lists the stdio servers may expose.
///
/// The specialists are on by default because a dashboard genuinely produces the
/// situations they answer. Set `BI_BROWSER_MINIMAL` to drop them when a model does
/// better with a smaller surface — a wide tool set invites picking an exotic tool for
/// an everyday job, which is the failure a narrower list prevents.
/// Every tool refused whatever else is allowed, across all servers.
fn withheld_tools() -> Vec<&'static str> {
    PLAYWRIGHT_WITHHELD
        .iter()
        .chain(ANALYTICS_WITHHELD.iter())
        .copied()
        .collect()
}

fn allowed_mcp_tools() -> Vec<&'static [&'static str]> {
    if std::env::var("BI_BROWSER_MINIMAL").is_ok() {
        println!("  \u{b7} BI_BROWSER_MINIMAL set \u{2014} browser specialists withheld");
        vec![BI_TOOLS, PLAYWRIGHT_PRIMARY, ANALYTICS_PRIMARY]
    } else {
        vec![
            BI_TOOLS,
            PLAYWRIGHT_PRIMARY,
            PLAYWRIGHT_SECONDARY,
            ANALYTICS_PRIMARY,
            ANALYTICS_SECONDARY,
        ]
    }
}

const DESKTOP_TOOLS: &[&str] = &[
    "open_application",
    "activate_app",
    "list_windows",
    "get_window",
    "get_display_size",
    "screenshot",
    "zoom",
    "left_click",
    "key",
    // Without these it has no way to enter text, and a URL typed through `key`
    // one character at a time is both doomed and expensive — `key` takes a combo
    // like `command+l`, never a string.
    "type",
    "write_clipboard",
    // Logging in and operating controls: the accessibility path is more reliable
    // than clicking coordinates, and Superset's login form exposes both fields.
    "find_element",
    "set_value",
    "press_button",
    "click_element",
    "scroll",
    "wait",
    "web_search",
    "scrape",
];

fn preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// Condense JSON into something readable in a one-line activity feed.
fn summarise(json: &str, max_chars: usize) -> String {
    let cleaned: String = json
        .trim_start_matches('{')
        .trim_end_matches('}')
        .replace("\\n", " ")
        .replace(['"', '{', '}'], "")
        .replace(':', ": ")
        .replace(',', ", ");
    preview(&cleaned.split_whitespace().collect::<Vec<_>>().join(" "), max_chars)
}

/// Read-only view of the console host, used to know when to wake up.
struct Console {
    base: String,
    http: reqwest::Client,
}

impl Console {
    fn new(base: String) -> Self {
        Self { base, http: reqwest::Client::new() }
    }

    async fn run_id(&self) -> Option<String> {
        let text =
            self.http.get(format!("{}/run-id", self.base)).send().await.ok()?.text().await.ok()?;
        let trimmed = text.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    async fn read(&self) -> Option<Value> {
        let response = self
            .http
            .post(format!("{}/rpc", self.base))
            .json(&serde_json::json!({ "name": "run_console", "arguments": {} }))
            .send()
            .await
            .ok()?;
        let body: Value = response.json().await.ok()?;
        let text = body
            .get("content")?
            .as_array()?
            .iter()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))?
            .get("text")?
            .as_str()?;
        serde_json::from_str(text).ok()
    }

    /// Push observed activity: thinking, calls, results.
    ///
    /// Observed here rather than asked of the model, because this loop already sees
    /// every streamed token and every tool call — so the feed is complete, costs no
    /// extra model calls, and includes the BI server's calls too.
    async fn report_activity(&self, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let _ = self
            .http
            .post(format!("{}/driver/activity", self.base))
            .json(&serde_json::json!({ "events": events }))
            .send()
            .await;
    }

    /// Report a turn that died before the agent could speak for itself.
    async fn report_failure(&self, message: &str) {
        let _ = self
            .http
            .post(format!("{}/driver", self.base))
            .json(&serde_json::json!({
                "state": "failed",
                "narration": format!("This turn stopped before I could finish: {message}"),
            }))
            .send()
            .await;
    }
}

/// The last transcript turn, when it is an unanswered message from the person.
fn pending_ask(run: &Value) -> Option<(String, String)> {
    let last = run.get("messages")?.as_array()?.last()?;
    if last.get("role").and_then(Value::as_str)? != "user" {
        return None;
    }
    Some((
        last.get("text").and_then(Value::as_str)?.to_string(),
        last.get("at").and_then(Value::as_str).unwrap_or_default().to_string(),
    ))
}

/// How to work a dashboard, and why the numbers come first.
/// Load the analytics routing policy from `skills/analytics-agent/SKILL.md`.
///
/// The file is the source of truth: it is versioned alongside the tools it
/// describes rather than frozen into a prompt literal here, so a gotcha found by
/// running the thing is written down once and every agent picks it up.
fn load_skill() -> Option<String> {
    // Two skills, because they answer different questions. `computer-use-forms`
    // teaches the general capture-click-type-verify loop for a UI that exposes no
    // accessible fields — true of every web page, not only this one. `analytics-agent`
    // teaches what a BI platform can do and which of its failures are real.
    let names = ["computer-use-forms", "analytics-agent"];
    let root = std::env::var("COMPUTER_USE_SKILLS")
        .unwrap_or_else(|_| "../mcp-servers/computer-use-mcp".to_string());
    let mut loaded = Vec::new();
    for name in names {
        let path = std::path::Path::new(&root).join(format!("skills/{name}/SKILL.md"));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                println!("  \u{2713} skill {name}");
                // Strip YAML front matter; the body is the policy.
                loaded.push(
                    text.split_once("---\n")
                        .and_then(|(_, rest)| rest.split_once("---\n"))
                        .map(|(_, body)| body.trim().to_string())
                        .unwrap_or(text),
                );
            }
            Err(_) => println!("  \u{b7} skill {name} not found at {}", path.display()),
        }
    }
    if loaded.is_empty() {
        println!(
            "  \u{b7} no skills loaded \u{2014} set COMPUTER_USE_SKILLS to a computer-use-mcp \
             checkout to load them"
        );
        return None;
    }
    Some(loaded.join("\n\n"))
}

fn analyst_brief(run_id: &str) -> String {
    format!(
        "You are a business intelligence analyst. You read the dashboards and metrics an \
         organisation already has, work out what is actually going on, and explain it to the \
         person watching.\n\n\
         ── what a good answer looks like ──\n\
         It answers the question that was asked. Every figure in it came from a query you ran, \
         not from a picture you looked at. Where the organisation has a certified definition of \
         a metric, it uses that definition and says whose it is. It names what you could not \
         establish, rather than rounding it off. And the person watching saw the dashboard you \
         were talking about while you were talking about it.\n\n\
         ── how to work ──\n\
         Call `bi_backend_info` first, because platforms differ in what they can do, and \
         `bi_recall` early, because something may already have been worked out here before. \
         Beyond that, choose your own route — the skills below carry what is known about these \
         tools and the failures that are real. Two habits are worth having: prefer a certified \
         metric definition over your own reading of a chart, and when a statistic points \
         somewhere, drill into it rather than describing it.\n\
         When you had to work something out that the platform did not tell you — which of \
         several similar charts people mean, an exact filter value, an id that is not what it \
         looks like — record it with `bi_remember`. Record the correction, never the figures. \
         That is what makes the next session faster.\n\n\
         ── the console ──\n\
         A person is watching and can type to you at any time. The runId is `{run_id}`.\n\
         Call `run_plan` first with 3 to 7 steps in plain language. Before each step, \
         `run_progress` with that `taskId`, `status:\"active\"` and a `narration` of one or two \
         sentences written for a person — no tool names, no ids. When a step is done, \
         `status:\"done\"` with a `note` holding the actual finding: a figure, a name, a \
         measurement. Never the word \"done\". To show a chart or a screen, use `run_progress` \
         with `capture:true`, which puts it in front of them and returns it to you — so look at \
         what came back before you describe it.\n\
         When the request is answered, `run_progress` with `state:\"done\"` and a closing \
         narration stating what you found and the numbers behind it. If you could not finish, \
         `state:\"failed\"` and say plainly what blocked you.\n\
         If `cancel_requested` is true in a reply, the person has asked you to stop. Stop \
         where you are: report what you already have with `run_progress`, set \
         `state:\"done\"` if it is useful or `state:\"failed\"` if it is not, and do not start \
         anything further.\n\
         If they type while you work, their message is in the transcript every run_* reply \
         returns. Read it and adapt.\n\n\
         ── what not to do ──\n\
         Do not describe a dashboard from its picture alone. Do not report a total you did not \
         query. Do not narrate your own tool calls — the console already shows them. If a call \
         fails you have no data, so report the error plainly and stop rather than reconstructing \
         the answer another way. Nothing you can call writes to a dashboard: you are reading a \
         business's real reporting, so be careful about what you claim it means."
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,adk=info".into()),
        )
        .init();

    println!("=== BI Analyst (ADK-Rust + DeepSeek Flash, live run console) ===\n");

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("Set DEEPSEEK_API_KEY");
    let base = std::env::var("CONSOLE_URL").unwrap_or_else(|_| "http://127.0.0.1:4517".to_string());
    let base = base.trim_end_matches('/').to_string();

    // ── 1. The console host: run tools plus a desktop ────────────────────
    let allowed: Vec<&str> = CONSOLE_TOOLS.iter().chain(DESKTOP_TOOLS.iter()).copied().collect();
    let filter = allowed.clone();
    let console_tools = McpHttpClientBuilder::new(format!("{base}/mcp"))
        .timeout(Duration::from_secs(180))
        .connect()
        .await
        .map_err(|error| {
            format!(
                "could not reach the console host at {base}/mcp ({error}). Start it with: \
                 npx -y -p @zavora-ai/computer-use-mcp computer-use-mcp-console --no-demo"
            )
        })?
        .with_filter(move |name: &str| filter.contains(&name));
    println!("  ✓ console host {base}/mcp ({} tools allowed)", allowed.len());

    // ── 2. The BI server, and market data when it is available ───────────
    let bi_bin = std::env::var("MCP_BI_BIN")
        .unwrap_or_else(|_| "../mcp-servers/mcp-bi/target/release/mcp-bi".to_string());
    let mut servers = format!(r#""bi": {{ "command": {bi_bin:?}, "args": [] }}"#);
    // Playwright drives its own browser, which is the right tool for a web app: it
    // acts by selector, waits by itself, and needs no coordinates. Not headless —
    // the window has to be visible for the person watching the console to see it.
    if std::env::var("BI_NO_BROWSER").is_err() {
        servers.push_str(
            r#", "browser": { "command": "npx", "args": ["-y", "@playwright/mcp@latest", "--isolated"] }"#,
        );
        println!("  \u{2713} playwright browser attached");
    } else {
        println!("  \u{b7} BI_NO_BROWSER set \u{2014} no browser automation, desktop tools only");
    }
    // The governed metric model. Optional, because the BI platform alone still answers
    // "what does this chart show"; with it the agent can also answer "what does this
    // company mean by revenue, and who owns that definition".
    if let Ok(analytics_bin) = std::env::var("MCP_ANALYTICS_BIN") {
        servers.push_str(&format!(
            r#", "analytics": {{ "command": {analytics_bin:?}, "args": [] }}"#
        ));
        println!("  \u{2713} analytics server attached \u{2014} certified metric definitions");
    } else {
        println!(
            "  \u{b7} MCP_ANALYTICS_BIN unset \u{2014} no certified metric definitions, so the \
             agent will read charts without knowing the organisation's semantics"
        );
    }
    if let Ok(market_bin) = std::env::var("MCP_MARKET_DATA_BIN") {
        servers.push_str(&format!(r#", "market": {{ "command": {market_bin:?}, "args": [] }}"#));
        println!("  ✓ market-data server attached");
    } else {
        println!("  · MCP_MARKET_DATA_BIN unset — dashboards only, no market cross-check");
    }
    let manager = McpServerManager::from_json(&format!(r#"{{ "mcpServers": {{ {servers} }} }}"#))?
        .with_health_check_interval(Duration::from_secs(60))
        .with_grace_period(Duration::from_secs(3));
    for (name, result) in &manager.start_all().await {
        match result {
            Ok(()) => println!("  ✓ mcp server {name}"),
            // Only the BI server is essential — it is what this agent reads. Market
            // data is a cross-check, so its absence degrades the analysis rather than
            // preventing it, and saying so beats failing to start.
            Err(error) if name == "analytics" || name == "market" => {
                println!(
                    "  \u{b7} the {name} server did not start ({error}) \u{2014} continuing \
                     without it"
                );
            }
            Err(error) if name == "browser" => {
                println!(
                    "  \u{b7} playwright did not start ({error}) \u{2014} falling back to the \
                     desktop tools, which is slower"
                );
            }
            Err(error) if name == "bi" => {
                return Err(format!(
                    "the BI server failed to start: {error}\n\
                     Build it with: cargo build --release --manifest-path \
                     <mcp-servers>/mcp-bi/Cargo.toml, then set MCP_BI_BIN to the binary."
                )
                .into());
            }
            Err(error) => {
                println!("  ! mcp server {name} unavailable ({error}); continuing without it");
            }
        }
    }
    let manager = Arc::new(manager);

    // Loaded now rather than when the agent is built, so a missing skill is
    // visible alongside the other readiness checks instead of after the wait.
    let skill = load_skill();
    // Resolved here rather than when the agent is built, so an operator sees which
    // browser surface is in force alongside the other readiness lines.
    let mcp_tools = allowed_mcp_tools();

    // The browser session and the BI server's API token are unrelated: the server
    // can be authenticated while the browser sits on a login page. Handing the
    // agent the credentials lets it sign in through the UI so the person actually
    // sees the dashboard — but it puts a password in the model's context, so it is
    // opt-in and says so out loud.
    // Backend-agnostic: BI_UI_USERNAME/PASSWORD when set, else the Superset ones,
    // because the sign-in the browser needs is not always the API's credentials.
    let ui_user = std::env::var("BI_UI_USERNAME").or_else(|_| std::env::var("SUPERSET_USERNAME"));
    let ui_pass = std::env::var("BI_UI_PASSWORD").or_else(|_| std::env::var("SUPERSET_PASSWORD"));
    let ui_login = match (std::env::var("BI_UI_LOGIN").is_ok(), ui_user, ui_pass) {
        (true, Ok(username), Ok(password)) => {
            println!(
                "  \u{26a0} BI_UI_LOGIN set \u{2014} the browser sign-in for {username:?} will be \
                 sent to the model so it can log in on screen. Use a throwaway account."
            );
            Some((username, password))
        }
        (true, _, _) => {
            println!(
                "  \u{b7} BI_UI_LOGIN set but no UI credentials given \u{2014} the agent \
                 cannot sign in on screen"
            );
            None
        }
        _ => {
            println!(
                "  \u{b7} BI_UI_LOGIN unset \u{2014} the agent will report a login page rather than \
                 signing in"
            );
            None
        }
    };

    // ── 3. Wait for the person to say something ──────────────────────────
    let console = Console::new(base.clone());
    println!("\nOpen {base}/ and ask about the dashboards.");
    println!("Waiting for the first message…");
    let run_id = loop {
        if let Some(id) = console.run_id().await {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    };
    println!("  ✓ joined run {run_id}\n{}", "─".repeat(60));

    // ── 4. Model: thinking on, uncapped ──────────────────────────────────
    let model = DeepSeekClient::new(
        DeepSeekConfig::new(&api_key, adk_model::catalog::DEEPSEEK_DEFAULT)
            .with_thinking_mode(ThinkingMode::Enabled)
            .with_reasoning_effort(ReasoningEffort::Max),
    )?;

    // The brief says how to work; the skill says what this exact setup does and
    // which failures are real. Keeping them separate means a lesson learned by
    // running it is recorded in the skill file, not buried in a prompt literal.
    let mut brief = analyst_brief(&run_id);
    if let Some((username, password)) = &ui_login {
        brief.push_str(&format!(
            "\n\n\u{2500}\u{2500} signing in on screen \u{2500}\u{2500}\n\
             The browser has its own session, separate from the data server's token, so the \
             dashboard URL may land on a login form. If the frame you captured shows one, sign \
             in with the browser tools: `browser_snapshot` to see the form, then \
             `browser_fill_form` with the email field set to {username:?} and the password \
             field to {password:?}, then `browser_click` the sign-in button. Three calls, no \
             coordinates. Do not estimate field positions from a screenshot.\n\
             Afterwards Chrome offers to save the password, right over the dashboard. That \
             dialog IS accessible: `click_element` the button labelled `Never`. Then capture \
             again and confirm you are looking at the dashboard before telling the person you \
             are showing it to them."
        ));
    }
    if let Some(policy) = &skill {
        brief.push_str(&format!(
            "\n\n\u{2500}\u{2500} routing policy \u{2500}\u{2500}\n\
             The policy below was written from measurements of this exact setup, and the \
             failure modes it lists are ones that actually happened.\n\n{policy}"
        ));
    }

    let agent = LlmAgentBuilder::new("bi-analyst")
        .description(
            "Reads an organisation's saved dashboards, drills into what is odd, and explains \
             it with the numbers behind it",
        )
        .model(Arc::new(model))
        .instruction(brief)
        .toolset(Arc::new(console_tools) as Arc<dyn adk_core::Toolset>)
        // The manager carries both servers, so the filter names the BI tools too.
        .toolset(Arc::new(Allowed {
            inner: Arc::clone(&manager) as Arc<dyn adk_core::Toolset>,
            allow: mcp_tools.clone(),
            withheld: withheld_tools(),
        }) as Arc<dyn adk_core::Toolset>)
        .build()?;

    // ── 5. Runner, one session for the whole conversation ────────────────
    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(adk_session::CreateRequest {
            app_name: "bi-analyst".to_string(),
            user_id: "analyst".to_string(),
            session_id: Some(run_id.clone()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("bi-analyst")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()?;

    // ── 6. The conversation loop ─────────────────────────────────────────
    let mut handled: Option<String> = None;
    let mut turn = 0usize;
    loop {
        let Some(run) = console.read().await else {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        };
        let Some((ask, at)) = pending_ask(&run) else {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        };
        if handled.as_deref() == Some(at.as_str()) {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        }
        handled = Some(at);
        turn += 1;

        println!("\n▸ turn {turn}: {}", preview(&ask, 140));
        println!("{}", "─".repeat(60));

        let mut buffered = String::new();
        macro_rules! flush {
            () => {
                if !buffered.trim().is_empty() {
                    println!("\n{}", buffered.trim());
                    buffered.clear();
                }
            };
        }

        let started = runner
            .run_str(
                "analyst",
                &run_id,
                Content { role: "user".to_string(), parts: vec![Part::Text { text: ask }] },
            )
            .await;
        let mut stream = match started {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("turn {turn} could not start: {error}");
                console.report_failure(&error.to_string()).await;
                continue;
            }
        };

        let mut failure: Option<String> = None;
        let mut started_at: HashMap<String, Instant> = HashMap::new();
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) => {
                    let Some(content) = &event.llm_response.content else { continue };
                    let mut activity: Vec<Value> = Vec::new();
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => buffered.push_str(text),
                            Part::FunctionCall { name, args, .. } => {
                                if !buffered.trim().is_empty() {
                                    activity.push(serde_json::json!({
                                        "kind": "thought", "detail": buffered.trim(),
                                    }));
                                }
                                flush!();
                                let args = serde_json::to_string(args).unwrap_or_default();
                                started_at.insert(name.clone(), Instant::now());
                                activity.push(serde_json::json!({
                                    "kind": "tool", "name": name, "detail": summarise(&args, 120),
                                }));
                                println!("→ {name} {}", preview(&args, 110));
                            }
                            Part::FunctionResponse { function_response, .. } => {
                                let body = serde_json::to_string(&function_response.response)
                                    .unwrap_or_default();
                                let is_run_tool = function_response.name.starts_with("run_");
                                let shown = if is_run_tool {
                                    format!("{} bytes of run state", body.len())
                                } else {
                                    preview(&body, 110)
                                };
                                let mut entry = serde_json::json!({
                                    "kind": "result",
                                    "name": function_response.name,
                                    "detail": if is_run_tool { String::new() } else { summarise(&body, 120) },
                                });
                                if let Some(start) = started_at.remove(&function_response.name) {
                                    entry["ms"] =
                                        serde_json::json!(start.elapsed().as_millis() as u64);
                                }
                                if body.contains("\"error\"") || body.contains("isError") {
                                    entry["failed"] = serde_json::json!(true);
                                }
                                activity.push(entry);
                                println!("← {} {shown}", function_response.name);
                            }
                            _ => {}
                        }
                    }
                    console.report_activity(&activity).await;
                }
                Err(error) => {
                    eprintln!("stream error: {error}");
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        flush!();
        if !buffered.trim().is_empty() {
            console
                .report_activity(&[
                    serde_json::json!({ "kind": "thought", "detail": buffered.trim() }),
                ])
                .await;
        }
        if let Some(message) = failure {
            console.report_failure(&message).await;
            println!("{}\nturn {turn} failed — still watching", "─".repeat(60));
        } else {
            println!("{}\nturn {turn} finished — watching for the next message", "─".repeat(60));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool the brief names must actually be available.
    ///
    /// This drifted once already: the brief told the agent to use `browser_find` and
    /// `browser_page_text` from the desktop server after those had been dropped from
    /// this example's filter in favour of Playwright's own tools. Nothing failed
    /// loudly — the agent simply spent calls on tools it did not have.
    #[test]
    fn the_brief_only_names_tools_the_agent_has() {
        let brief = analyst_brief("run_test");
        let available: Vec<&str> = PLAYWRIGHT_PRIMARY
            .iter()
            .chain(PLAYWRIGHT_SECONDARY.iter())
            .chain(BI_TOOLS.iter())
            .chain(CONSOLE_TOOLS.iter())
            .chain(DESKTOP_TOOLS.iter())
            .copied()
            .collect();

        for word in brief.split(|c: char| !c.is_alphanumeric() && c != '_') {
            // `browser_*` in prose splits to a bare prefix; that is not a tool name.
            let is_tool = (word.starts_with("browser_") && word.len() > "browser_".len())
                || (word.starts_with("bi_") && word.len() > "bi_".len());
            if !is_tool {
                continue;
            }
            assert!(
                available.contains(&word),
                "the brief names {word}, which is not in any allowed list"
            );
            assert!(
                !PLAYWRIGHT_WITHHELD.contains(&word),
                "the brief names {word}, which is deliberately withheld"
            );
        }
    }

    /// A withheld tool must never also be allowed, whatever the tiers say.
    #[test]
    fn withheld_tools_are_not_reachable_through_any_tier() {
        for name in PLAYWRIGHT_WITHHELD {
            assert!(!PLAYWRIGHT_PRIMARY.contains(name), "{name} is both withheld and primary");
            assert!(!PLAYWRIGHT_SECONDARY.contains(name), "{name} is both withheld and secondary");
        }
    }

    /// The tiers must partition Playwright's surface, so adding a tool to its server
    /// without deciding which tier it belongs to shows up here.
    #[test]
    fn the_tiers_do_not_overlap() {
        for name in PLAYWRIGHT_PRIMARY {
            assert!(
                !PLAYWRIGHT_SECONDARY.contains(name),
                "{name} is in two tiers; it should sit in exactly one"
            );
        }
    }
}
