//! # Blender Console — an agent a person can watch, and interrupt
//!
//! A live multi-turn agent driving the `computer-use-mcp` run console. Someone
//! opens the console in a browser, types what they want, and watches the agent
//! plan it, work on their desktop, and tick tasks off as frames land.
//!
//! ## The shape of it
//!
//! ```text
//!   browser ──┐
//!             │  console host (one Node process: one run store, one desktop session)
//!   this ─────┘        │
//!   agent   /mcp       └── the run: transcript, plan, task states, last frame
//! ```
//!
//! The host is the single owner of the run. This agent connects to its `/mcp`
//! endpoint over Streamable HTTP, which hands it the desktop tools *and* the
//! `run_*` console tools from the same server — so a plan it declares and a
//! screenshot it captures land in the very store the browser is polling. Nothing
//! is mirrored between two places, because there is only one place.
//!
//! Blender's own MCP server is attached separately over stdio when
//! `BLENDER_MCP_BIN` is set. Two toolsets, one agent.
//!
//! ## Multi-turn, without a queue
//!
//! One run is one conversation. The person's first message opens it; anything
//! they type later is appended to the same transcript. This agent decides there
//! is work to do by exactly one rule:
//!
//! > the last message in the transcript has `role: "user"`
//!
//! Answering appends an `agent` turn, which clears the condition. That is the
//! whole protocol — no queue, no callback, no second channel. Because every
//! `run_*` reply carries the transcript, an agent already mid-task sees a new
//! instruction on its next call and can change course without being asked twice.
//!
//! Conversation memory is the session service's job: every turn runs against the
//! same session id, so turn three still knows what was built in turn one.
//!
//! ## Setup
//!
//! ```bash
//! # 1. the console host — no checkout of computer-use-mcp needed
//! npx -y -p @zavora-ai/computer-use-mcp computer-use-mcp-console --no-demo
//!
//! # 2. this agent
//! export DEEPSEEK_API_KEY=...
//! export BLENDER_MCP_BIN=/path/to/official/blender-mcp   # optional
//! cargo run --manifest-path examples/blender_console/Cargo.toml
//! ```
//!
//! Then open <http://127.0.0.1:4517/> and type what you want built. This process
//! waits for that first message. `--no-demo` matters: without it the host drives a
//! scripted walkthrough of its own, which this agent would then try to answer.
//!
//! Optional: set `COMPUTER_USE_SKILLS` to a computer-use-mcp checkout to load the
//! Blender routing policy from `skills/blender-agent/SKILL.md`. Without it the
//! agent still runs, just without that policy.

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Part};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort, ThinkingMode};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use adk_tool::mcp::{McpHttpClientBuilder, manager::McpServerManager};
use futures::StreamExt;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The console tools. These are the run itself: without them the person watching
/// sees nothing, so they are the one group the agent must always be able to call.
const CONSOLE_TOOLS: &[&str] = &["run_plan", "run_progress", "run_say", "run_console"];

/// Desktop tools this workflow may reach, out of the 71 the host exposes.
///
/// Deliberately no `run_script`, no `filesystem`, no `process_kill`: Blender's
/// own server already executes arbitrary Python, so widening the desktop surface
/// on top of that buys nothing and costs a great deal. `run_start` is excluded
/// too — the host owns run creation, and a second run would be one the browser
/// is not watching.
const DESKTOP_TOOLS: &[&str] = &[
    "open_application",
    "activate_app",
    "list_windows",
    "get_window",
    "get_display_size",
    "get_frontmost_app",
    "screenshot",
    "zoom",
    "left_click",
    "key",
    "mouse_drag",
    "scroll",
    "wait",
];

fn preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

/// Load the Blender routing policy from the computer-use-mcp checkout.
///
/// The file is the source of truth: it is versioned alongside the tools it
/// describes rather than frozen into a prompt literal here.
fn load_skill() -> Option<String> {
    let root = std::env::var("COMPUTER_USE_SKILLS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("../computer-use-mcp"));
    let path = root.join("skills/blender-agent/SKILL.md");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            println!("  ✓ routing policy from {}", path.display());
            // Strip YAML front matter; the body is the policy.
            Some(
                text.split_once("---\n")
                    .and_then(|(_, rest)| rest.split_once("---\n"))
                    .map(|(_, body)| body.trim().to_string())
                    .unwrap_or(text),
            )
        }
        Err(error) => {
            println!("  ! no routing policy at {} ({error})", path.display());
            None
        }
    }
}

/// Read-only view of the console host, used to know when to wake up.
///
/// The agent does all of its *writing* through MCP tools. This is only for
/// watching, which is why it needs nothing but two plain GET/POSTs.
struct Console {
    base: String,
    http: reqwest::Client,
}

impl Console {
    fn new(base: String) -> Self {
        Self { base, http: reqwest::Client::new() }
    }

    /// The current conversation, or `None` until the person says something.
    async fn run_id(&self) -> Option<String> {
        let text =
            self.http.get(format!("{}/run-id", self.base)).send().await.ok()?.text().await.ok()?;
        let trimmed = text.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// The run as the browser sees it: transcript, plan, narration, last frame.
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

    /// Report a turn that died before the agent could speak for itself.
    ///
    /// A model error would otherwise leave the console frozen mid-task with no
    /// explanation, which is the one failure a watching person cannot diagnose.
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
///
/// Returns its timestamp as well, because that is what makes an ask identifiable:
/// the caller records it so the same message is never worked twice.
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

/// How the agent is expected to use the console.
///
/// This is the contract that turns a working agent into a *watchable* one. It is
/// spelled out in behavioural terms — when to call what, and what a good note
/// looks like — because "keep the user informed" reliably produces nothing.
fn console_protocol(run_id: &str) -> String {
    format!(
        "── the console ──\n\
         A person is watching this run in a live console, and can type to you at any time.\n\
         The runId for this conversation is `{run_id}`. Every run_* call takes it.\n\n\
         Each time you are given something to do:\n\n\
         1. Call `run_plan` FIRST, with 3 to 7 steps in the order you will do them, titled \
            so a non-expert can read them. The plan is all the person has to look at before \
            work starts, so decide the shape of the job before you touch anything.\n\
         2. Before you begin a step, call `run_progress` with that `taskId`, \
            `status:\"active\"`, and a `narration` of one or two plain sentences saying what \
            you are about to do. Write it for a person, not a log: no tool names, no JSON, \
            no ids.\n\
         3. When a step is genuinely finished, call `run_progress` with `status:\"done\"` and \
            a `note` holding the real result — a count, a name, a measurement, a path. Never \
            the word \"done\".\n\
         4. To look at the screen, use `run_progress` with `capture:true` (plus `window_id` \
            when the thing you care about is inside one window). It hands the frame back to \
            you as an image *and* puts it in front of the person. This is the only way they \
            ever see anything: a plain `screenshot` call shows them nothing, so prefer \
            capturing through `run_progress` and keep `screenshot` for frames nobody needs \
            to see. Capture at the moments someone would actually want to look — after the \
            screen changed, not on every call.\n\
         5. When the whole request is satisfied, call `run_progress` with `state:\"done\"` and \
            a closing narration that says what now exists. If you cannot finish, use \
            `state:\"failed\"` and say plainly what blocked you.\n\n\
         If the person types while you are working, their message is in the transcript that \
         every run_* reply hands back. Read it and adapt: answer a question with `run_say`, \
         and if they change what they want, call `run_plan` again to revise the steps rather \
         than quietly doing something else.\n\n\
         `run_progress` is the only thing the person can see. A task you marked done must \
         actually be done, and a narration must describe what happened rather than what you \
         intended."
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

    println!("=== Blender Console (ADK-Rust + DeepSeek Flash, live run console) ===\n");

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("Set DEEPSEEK_API_KEY");
    let base = std::env::var("CONSOLE_URL").unwrap_or_else(|_| "http://127.0.0.1:4517".to_string());
    let base = base.trim_end_matches('/').to_string();

    // ── 1. The console host: desktop tools and the run tools, one connection ──
    let allowed: Vec<&str> = CONSOLE_TOOLS.iter().chain(DESKTOP_TOOLS.iter()).copied().collect();
    let filter = allowed.clone();
    let console_tools = McpHttpClientBuilder::new(format!("{base}/mcp"))
        .timeout(Duration::from_secs(180))
        .connect()
        .await
        .map_err(|error| {
            format!("could not reach the console host at {base}/mcp ({error}). Start it with: node agents/run-console-host.mjs")
        })?
        .with_filter(move |name: &str| filter.contains(&name));
    println!("  ✓ console host {base}/mcp ({} tools allowed)", allowed.len());

    // ── 2. Blender's own server, when one is configured ──────────────────
    let skill = load_skill();
    let blender = match std::env::var("BLENDER_MCP_BIN") {
        Ok(bin) => {
            let config = format!(
                r#"{{ "mcpServers": {{ "blender": {{ "command": {bin:?}, "args": ["--transport", "stdio"] }} }} }}"#
            );
            let manager = McpServerManager::from_json(&config)?
                .with_health_check_interval(Duration::from_secs(60))
                .with_grace_period(Duration::from_secs(3));
            for (name, result) in &manager.start_all().await {
                match result {
                    Ok(()) => println!("  ✓ mcp server {name}"),
                    Err(error) => {
                        return Err(format!("MCP server '{name}' failed to start: {error}").into());
                    }
                }
            }
            Some(Arc::new(manager))
        }
        Err(_) => {
            println!("  · BLENDER_MCP_BIN unset — desktop tools only, no Blender server");
            None
        }
    };

    // ── 3. Wait for the person to say something ──────────────────────────
    //
    // The host opens the run from their first message, so its prompt is their own
    // words. Until then there is no conversation to join.
    let console = Console::new(base.clone());
    println!("\nOpen {base}/ and type what you want built.");
    println!("Waiting for the first message…");
    let run_id = loop {
        if let Some(id) = console.run_id().await {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    };
    println!("  ✓ joined run {run_id}\n{}", "─".repeat(60));

    // ── 4. Model: thinking on, uncapped ──────────────────────────────────
    //
    // Reasoning is billed against max_tokens and a dense screenshot can need
    // several thousand tokens before the first word of the answer — one Blender
    // window took 6,959. Capping it truncates the answer and still bills in full,
    // so no cap is set.
    let model = DeepSeekClient::new(
        DeepSeekConfig::new(&api_key, adk_model::catalog::DEEPSEEK_DEFAULT)
            .with_thinking_mode(ThinkingMode::Enabled)
            .with_reasoning_effort(ReasoningEffort::Max),
    )?;

    // ── 5. The agent ─────────────────────────────────────────────────────
    let mut instruction = format!(
        "You operate this person's real desktop, and you narrate what you are doing into a \
         console they are watching.\n\n{}",
        console_protocol(&run_id)
    );
    if let Some(policy) = &skill {
        instruction.push_str(&format!(
            "\n\n── routing policy ──\n\
             Tools are namespaced by the server that owns them where names collide: Blender \
             tools act inside Blender, desktop tools act on the operating system. The policy \
             below was written from measurements of this exact setup and the failure modes it \
             lists are real.\n\n{policy}"
        ));
    }

    let mut builder = LlmAgentBuilder::new("blender-console")
        .description("Builds things on a live desktop while reporting a plan, progress and screenshots to a watching person")
        .model(Arc::new(model))
        .instruction(instruction)
        .toolset(Arc::new(console_tools) as Arc<dyn adk_core::Toolset>);
    if let Some(manager) = &blender {
        builder = builder.toolset(Arc::clone(manager) as Arc<dyn adk_core::Toolset>);
    }
    let agent = builder.build()?;

    // ── 6. Runner, with one session for the whole conversation ───────────
    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(adk_session::CreateRequest {
            app_name: "blender-console".to_string(),
            user_id: "artist".to_string(),
            session_id: Some(run_id.clone()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("blender-console")
        .agent(Arc::new(agent) as Arc<dyn Agent>)
        .session_service(session_service.clone())
        .build()?;

    // ── 7. The conversation loop ─────────────────────────────────────────
    //
    // `handled` is the timestamp of the ask last worked. It is what stops the same
    // message being answered twice when a turn ends without the agent writing to
    // the transcript.
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

        // A turn that fails must not take the process down: the person is still
        // watching, and their next message deserves an agent that is still here.
        let started = runner
            .run_str(
                "artist",
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
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) => {
                    let Some(content) = &event.llm_response.content else { continue };
                    for part in &content.parts {
                        match part {
                            Part::Text { text } => buffered.push_str(text),
                            Part::FunctionCall { name, args, .. } => {
                                flush!();
                                let args = serde_json::to_string(args).unwrap_or_default();
                                println!("→ {name} {}", preview(&args, 110));
                            }
                            Part::FunctionResponse { function_response, .. } => {
                                // Console replies carry the whole run, screenshot included.
                                // Printing that would bury the log in base64.
                                let body = serde_json::to_string(&function_response.response)
                                    .unwrap_or_default();
                                let shown = if function_response.name.starts_with("run_") {
                                    format!("{} bytes of run state", body.len())
                                } else {
                                    preview(&body, 110)
                                };
                                println!("← {} {shown}", function_response.name);
                            }
                            _ => {}
                        }
                    }
                }
                Err(error) => {
                    eprintln!("stream error: {error}");
                    failure = Some(error.to_string());
                    break;
                }
            }
        }
        flush!();
        if let Some(message) = failure {
            console.report_failure(&message).await;
            println!(
                "{}\nturn {turn} failed — still watching for the next message",
                "─".repeat(60)
            );
        } else {
            println!("{}\nturn {turn} finished — watching for the next message", "─".repeat(60));
        }
    }
}
