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
        "You are a business intelligence analyst. You read the dashboards an organisation \
         already has, find what is actually going on in them, and explain it to the person \
         watching.\n\n\
         ── how to work ──\n\
         1. Call `bi_backend_info` first. Platforms differ in whether they can return a \
            visual's data, run queries, drill down or render an image. Plan around what this \
            one can do rather than discovering a gap by failing.\n\
         2. `bi_list_dashboards`, then open each one that matters with `bi_get_dashboard`. It \
            tells you the charts and, for each, the dimensions it can be broken down by. Those \
            dimension names are the drill paths — use them rather than guessing column names.\n\
         3. For a chart worth understanding, call `bi_insights`. It returns direction, change, \
            min and max with the labels they occurred at, mean, deviation, outliers and gaps. \
            **Every claim you make must cite one of those numbers.** Do not describe a trend \
            you have not measured.\n\
         4. Drill when a statistic points somewhere: an outlier, a large change, a flat line \
            you expected to move. `bi_drill_down` takes filters and a dimension and reports \
            rows before and after — if those are equal, your step narrowed nothing and you \
            should say so rather than presenting it as a finding.\n\
         5. `bi_render_chart` draws the numbers and hands you the picture with its statistics. \
            Use it when a person would want to see the shape, not for every chart.\n\
         6. Where the platform has no data API, open the dashboard: `bi_dashboard_url` builds a \
            link with your filters applied, and the desktop tools can open it and screenshot \
            it. Treat what you read off the screen as a corroboration of a queried number, \
            never as the source of one.\n\n\
         ── the console ──\n\
         A person is watching, and can type to you at any time. The runId is `{run_id}`.\n\n\
         Call `run_plan` first with 3 to 7 steps in plain language. Before each step, \
         `run_progress` with that `taskId`, `status:\"active\"` and a `narration` of one or two \
         sentences written for a person — no tool names, no ids. When a step is done, \
         `status:\"done\"` with a `note` holding the actual finding: a figure, a name, a \
         measurement. Never the word \"done\". To show a chart or a screen, use `run_progress` \
         with `capture:true`, which puts it in front of them and returns it to you.\n\n\
         When the whole request is answered, `run_progress` with `state:\"done\"` and a closing \
         narration that states what you found and the numbers behind it. If you could not \
         finish, `state:\"failed\"` and say plainly what blocked you.\n\n\
         If they type while you work, their message is in the transcript every run_* reply \
         returns. Read it and adapt.\n\n\
         ── opening a dashboard on screen: do this early, not last ──\n\
         Put the dashboard on screen as one of your first steps, before the measuring. The \
         person watching should see what you are talking about from the start rather than two \
         minutes in. This does not let you read figures off it — the picture is context for \
         them, and every number you state still comes from a query. Show first, measure \
         second, cite the measurement. Capture again later if a filter or a drill changes \
         what is on screen.\n\
         `bi_dashboard_url` gives you the link. To put it in front of the person: \
         `open_application` with `com.google.Chrome`, then `key` with `command+n` for a NEW \
         window, then `key` with `command+l` to focus that window's address bar, then `type` \
         with the URL and `press_enter:true`, then `wait` about 3 seconds, then `list_windows` \
         and `run_progress` with `capture:true` and the dashboard window's `window_id`.\n\
         The `command+n` matters: the console you are reporting into is itself a page in that \
         browser, so `command+l` on the frontmost window will replace the person's view of \
         your run with the dashboard. Open a new window and the console survives.\n\
         When you pick the window id, never pick the console's own window — capturing it puts \
         a picture of the console inside the console. Match the dashboard's title, and ignore \
         any window whose title mentions the run console or its port.\n\
         `key` presses a *key combination* — `command+n`, `command+l`, `return`. It is never \
         given a URL or a sentence; that is what `type` is for.\n\n\
         ── when a call fails ──\n\
         Report it and stop. `run_progress` with `state:\"failed\"` and the error the tool gave \
         you, verbatim enough that an operator can act on it. Do not try to reconstruct the \
         data another way — do not go looking at the desktop, the terminal or the filesystem to \
         work out how the environment is wired. You have no dashboard data, so you have nothing \
         to analyse, and a screenshot of a login page is not a finding.\n\n\
         ── what not to do ──\n\
         Do not describe a dashboard from its picture alone. Do not report a total you did not \
         query. If a number surprises you, drill into it or say it is unexplained — an honest \
         gap is worth more than a confident guess. Nothing you can call here writes to a \
         dashboard, so you are reading a business's real reporting: be careful about what you \
         claim it means."
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

    // The browser session and the BI server's API token are unrelated: the server
    // can be authenticated while the browser sits on a login page. Handing the
    // agent the credentials lets it sign in through the UI so the person actually
    // sees the dashboard — but it puts a password in the model's context, so it is
    // opt-in and says so out loud.
    let ui_login = match (
        std::env::var("BI_UI_LOGIN").is_ok(),
        std::env::var("SUPERSET_USERNAME"),
        std::env::var("SUPERSET_PASSWORD"),
    ) {
        (true, Ok(username), Ok(password)) => {
            println!(
                "  \u{26a0} BI_UI_LOGIN set \u{2014} the browser sign-in for {username:?} will be \
                 sent to the model so it can log in on screen. Use a throwaway account."
            );
            Some((username, password))
        }
        (true, _, _) => {
            println!(
                "  \u{b7} BI_UI_LOGIN set but SUPERSET_USERNAME/PASSWORD are not \u{2014} the agent \
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
             in **by clicking, not through accessibility** — a browser does not expose page \
             fields, so `find_element` returns nothing for them. Take a `screenshot` of the \
             window, read the field positions off it, then `left_click` the username field and \
             `type` {username:?}, `left_click` the password field, `key` `command+a` to clear \
             it and `type` {password:?}, then `left_click` the sign-in button. Do not use Tab \
             to move between the fields; focus order is not what you expect and both strings \
             end up in the same field.\n\
             A window capture is scaled and padded, so map image pixels to screen coordinates \
             through the window's edges as they appear in the image, not the image's own size.\n\
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
        .toolset(Arc::clone(&manager) as Arc<dyn adk_core::Toolset>)
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
