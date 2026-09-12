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
use run_console_driver::{Allowed, Budget, Console, Outcome, preview, redact_for_display, register_secret, response_failed, scrub_known_secrets, summarise};
use adk_tool::mcp::{McpHttpClientBuilder, manager::McpServerManager};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Console tools: the run itself. Without these nobody can see the work.
const CONSOLE_TOOLS: &[&str] =
    &["run_plan", "run_progress", "run_say", "run_console", "run_attachment"];

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
    // First, always: this server ships seeded fixtures whose metrics carry
    // `certified: true` and an owner's name. Without asking, an agent cannot tell
    // them from an organisation's real governed definitions — and one did not.
    "analytics_backend_info",
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

/// Market data, for cross-checking a dashboard figure against the outside world.
///
/// Read-only only. The server's writes — `create_instrument`, `set_quote`, `add_bar`,
/// `publish_mark` and the `create_*` family — publish marks and define instruments,
/// which is a different job from analysing a dashboard.
const MARKET_PRIMARY: &[&str] = &[
    "backend_info",
    "get_quote",
    "history",
    "analytics",
    "moving_average",
    "correlation",
    "fx_convert",
    "benchmark_level",
    "list_instruments",
    "get_instrument",
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

fn allowed_mcp_tools(attached: &Attached) -> Vec<&'static [&'static str]> {
    let minimal = std::env::var("BI_BROWSER_MINIMAL").is_ok();
    if minimal {
        println!("  \u{b7} BI_BROWSER_MINIMAL set \u{2014} browser specialists withheld");
    }
    // Only list tools for a server that actually started. Offering a name for a
    // server that is not there wastes a call discovering it, and the reverse — a
    // server running whose every tool is filtered out — was real: market-data was
    // being spawned with none of its tools allowed, so it could do nothing at all.
    let mut lists: Vec<&'static [&'static str]> = vec![BI_TOOLS];
    if attached.browser {
        lists.push(PLAYWRIGHT_PRIMARY);
        if !minimal {
            lists.push(PLAYWRIGHT_SECONDARY);
        }
    }
    if attached.analytics {
        lists.push(ANALYTICS_PRIMARY);
        if !minimal {
            lists.push(ANALYTICS_SECONDARY);
        }
    }
    if attached.market {
        lists.push(MARKET_PRIMARY);
    }
    lists
}

/// Which optional servers are actually running.
#[derive(Debug, Default, Clone, Copy)]
struct Attached {
    browser: bool,
    analytics: bool,
    market: bool,
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

/// Field names whose values must never be printed or fed to the console.
///
/// The agent is deliberately given a sign-in so it can reach a dashboard, which puts
/// the password in the model's context. That is a decided trade. What was *not*
/// decided is the same string travelling on to the terminal and to the activity feed
/// the person is watching, which is where it ends up in a log file or a screenshot.
/// Measured before this existed: a `browser_fill_form` call printed its fields
/// verbatim, and the password survived only because the email sorted first and the
/// preview happened to cut at 110 characters. A secret protected by truncation is not
/// protected.
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
         Call `bi_backend_info` and `analytics_backend_info` first. The second matters: \
         that server may be serving generated demo fixtures whose metrics carry \
         `certified: true` and an owner's name, and if its provenance is `demo` you must \
         say so every time you quote a figure from it rather than presenting it as this \
         organisation's number. Also call \
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
         Every reply carries `pending`: messages the person sent that you have not \
         answered. When you answer one, pass its `id` as `acknowledge` on `run_say` \
         \u{2014} that is what clears it. Nothing else does, and in particular a narration \
         does not.\n\
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
         the answer another way. The data tools cannot write, but the browser can: you are signed into a real \
         account and a click lands on whatever is under it, including Save, Delete and \
         Publish. Operate filters and drill-downs freely \u{2014} those change your view. Treat \
         anything that persists as out of scope unless you were asked for it, and if you \
         are unsure whether a control persists, do not click it and say why."
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
    let mut attached = Attached::default();
    if std::env::var("BI_NO_BROWSER").is_err() {
        attached.browser = true;
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
        attached.analytics = true;
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
        attached.market = true;
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
    let mcp_tools = allowed_mcp_tools(&attached);
    let budget = Budget::from_env("BI");

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
            // Registered so the value is scrubbed wherever it surfaces, including a
            // generic text field whose name announces nothing and a result that echoes
            // it back. Name-based rules alone cannot see either.
            register_secret(&password);
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
            // Every server's whole surface is named explicitly here, so nothing passes
            // by prefix.
            passthrough_prefix: None,
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
    let mut turn = 0usize;
    loop {
        let Some(_) = console.read().await else {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        };
        // Read from the console's own `pending`, which the host computes from message
        // ids and an acknowledgement cursor. Inferring it from the transcript is what
        // let a progress narration hide a question typed mid-run.
        let pending = console.pending().await;
        let Some(first) = pending.first() else {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        };
        let ask = first.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
        let ask_id = first.get("id").and_then(Value::as_i64).unwrap_or_default();
        if ask.is_empty() {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        }
        // No local dedup by timestamp: the host's `pending` already excludes anything
        // acknowledged, and acknowledging is what marks a question answered. A timestamp
        // key could not tell two messages sent in the same second apart, and duplicated
        // state the host already holds.
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
                // The id travels with the question, because the agent cannot acknowledge
                // a message it was never told the id of.
                Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text {
                        text: format!("[message {ask_id}] {ask}"),
                    }],
                },
            )
            .await;
        let mut stream = match started {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("turn {turn} could not start: {error}");
                console.finish(&Outcome::Failed(error.to_string()), "").await;
                continue;
            }
        };

        let mut failure: Option<String> = None;
        let mut cancelled = false;
        let mut stopped_short: Option<String> = None;
        let mut tool_calls: u64 = 0;
        let turn_started = Instant::now();
        let mut started_at: HashMap<String, std::collections::VecDeque<Instant>> = HashMap::new();
        while let Some(event) = stream.next().await {
            // Watch for a stop between events, not only when the model happens to call
            // a run tool. An agent that is mid-thought may not call one for a while, and
            // "Stopping…" on screen while tokens keep being spent is the failure this
            // avoids. Dropping the stream is what actually ends the work; the flag alone
            // only lets a cooperative agent notice.
            if !cancelled && console.cancel_requested().await {
                cancelled = true;
                println!("\n\u{23f9} the person asked to stop \u{2014} ending this turn");
                break;
            }
            // A bound that stops work silently is worse than no bound, because the
            // person cannot tell it from a finish. Record the reason and report it.
            if let Some(reason) = budget.exceeded(turn_started, tool_calls) {
                println!("\n\u{23f1} stopping: {reason}");
                stopped_short = Some(reason);
                break;
            }
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
                                tool_calls += 1;
                                started_at.entry(name.clone()).or_default().push_back(Instant::now());
                                // Redact before either path, not after: the console
                                // feed and the terminal are both places a secret
                                // outlives the run.
                                let safe = redact_for_display(&args);
                                activity.push(serde_json::json!({
                                    "kind": "tool", "name": name, "detail": summarise(&safe, 120),
                                }));
                                println!("→ {name} {}", preview(&safe, 110));
                            }
                            Part::FunctionResponse { function_response, .. } => {
                                let body = serde_json::to_string(&function_response.response)
                                    .unwrap_or_default();
                                let is_run_tool = function_response.name.starts_with("run_");
                                let shown = if is_run_tool {
                                    format!("{} bytes of run state", body.len())
                                } else {
                                    preview(&scrub_known_secrets(&body), 110)
                                };
                                let mut entry = serde_json::json!({
                                    "kind": "result",
                                    "name": function_response.name,
                                    "detail": if is_run_tool { String::new() } else { summarise(&scrub_known_secrets(&body), 120) },
                                });
                                // Oldest first: responses arrive in call order, so a
                                // second call to the same tool no longer inherits or
                                // discards the first one's start time.
                                if let Some(start) = started_at
                                    .get_mut(&function_response.name)
                                    .and_then(std::collections::VecDeque::pop_front)
                                {
                                    entry["ms"] =
                                        serde_json::json!(start.elapsed().as_millis() as u64);
                                }
                                // Read the structured result rather than searching its
                                // serialized text. `body.contains("isError")` matched
                                // `"isError": false` too, so successful calls were being
                                // reported as failures in the feed the person watches.
                                if response_failed(&function_response.response) {
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
        // Report before flushing, not after. flush!() clears the buffer, so the check
        // that followed it could never be true and the agent's closing summary reached
        // the terminal but never the console.
        let closing = buffered.trim().to_string();
        if !closing.is_empty() {
            console
                .report_activity(&[serde_json::json!({ "kind": "thought", "detail": &closing })])
                .await;
        }
        flush!();
        if let Some(message) = failure {
            console.finish(&Outcome::Failed(message.clone()), "").await;
            println!("{}\nturn {turn} failed — still watching", "─".repeat(60));
        } else {
            // A model can stop without ever calling run_progress with a terminal
            // state, which leaves the console reading "working" forever. The person
            // watching then cannot tell a finished run from a hung one, so the driver
            // closes the turn itself rather than trusting the agent to have done it.
            if let Some(reason) = stopped_short {
                console.finish(&Outcome::OutOfBudget(reason.clone()), &closing).await;
                println!("{}\nturn {turn} stopped short: {reason}", "─".repeat(60));
            } else if cancelled {
                // Say what was done before stopping, so a stop is not silent about the
                // work already paid for.
                console.finish(&Outcome::Cancelled, &closing).await;
                println!("{}\nturn {turn} stopped at the person's request", "─".repeat(60));
            } else {
                console
                    .finish(
                        if closing.is_empty() { &Outcome::Incomplete } else { &Outcome::Completed },
                        &closing,
                    )
                    .await;
                println!("{}\nturn {turn} finished — watching for the next message", "─".repeat(60));
            }
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
            .chain(ANALYTICS_PRIMARY.iter())
            .chain(ANALYTICS_SECONDARY.iter())
            .chain(MARKET_PRIMARY.iter())
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    /// The bug: `body.contains("isError")` matched `"isError": false`, so successful
    /// calls were reported as failures in the feed the person watches.
    #[test]
    fn a_successful_result_is_not_reported_as_failed() {
        let ok = serde_json::json!({
            "isError": false,
            "content": [{ "type": "text", "text": "{\"rows\": 28}" }]
        });
        assert!(!response_failed(&ok), "isError false is success");

        let plain = serde_json::json!({ "content": [{ "type": "text", "text": "28 rows" }] });
        assert!(!response_failed(&plain));

        // A body that merely mentions the word must not trip it either.
        let mentions = serde_json::json!({
            "content": [{ "type": "text", "text": "{\"note\": \"no error occurred\"}" }]
        });
        assert!(!response_failed(&mentions));
    }

    #[test]
    fn a_real_failure_is_still_caught_both_ways() {
        // The protocol flag.
        let flagged = serde_json::json!({ "isError": true, "content": [] });
        assert!(response_failed(&flagged));

        // And a domain error inside the content, which is how these servers report one.
        let domain = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "{\"error\": \"bi_error\", \"message\": \"HTTP 500\"}"
            }]
        });
        assert!(response_failed(&domain), "a domain error in the text still counts");
    }

    #[test]
    fn a_null_or_false_error_field_is_success() {
        // Servers commonly return `error: null` on success; treating that as a failure
        // would mark almost everything red.
        assert!(!response_failed(&serde_json::json!({ "error": serde_json::Value::Null })));
        assert!(!response_failed(&serde_json::json!({ "error": false })));
        assert!(response_failed(&serde_json::json!({ "error": "boom" })));
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn no_budget_never_stops_a_turn() {
        // The default, and it must stay the default: a capped reasoning budget produced
        // worse answers, and truncating a thought spends the tokens without buying the
        // conclusion.
        let budget = Budget { seconds: None, calls: None };
        assert!(budget.exceeded(Instant::now(), 10_000).is_none());
    }

    #[test]
    fn a_call_budget_stops_and_says_which_one() {
        let budget = Budget { seconds: None, calls: Some(5) };
        assert!(budget.exceeded(Instant::now(), 4).is_none());
        let reason = budget.exceeded(Instant::now(), 5).expect("should stop at the limit");
        assert!(reason.contains("5-call"), "the reason must name the budget: {reason}");
        assert!(reason.contains('s'), "and report elapsed time for context: {reason}");
    }

    #[test]
    fn a_time_budget_stops_and_reports_the_calls_made() {
        let budget = Budget { seconds: Some(1), calls: None };
        let long_ago = Instant::now() - std::time::Duration::from_secs(2);
        let reason = budget.exceeded(long_ago, 7).expect("should stop past the limit");
        assert!(reason.contains("1s time budget"), "{reason}");
        assert!(reason.contains("7 tool calls"), "what was done matters as much as why: {reason}");
    }
}
