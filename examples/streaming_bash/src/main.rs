//! # Streaming Bash — Live Terminal Output from LlmAgent Tools
//!
//! Demonstrates `emit_progress` on `ToolContext`: an LlmAgent with a `BashTool`
//! that streams stdout/stderr line-by-line as the command runs. Each chunk
//! rides the *same* `EventStream` as the model's reply.
//!
//! ## Run
//!
//! ```bash
//! # Web UI (default) — open http://localhost:3000
//! cargo run --manifest-path examples/streaming_bash/Cargo.toml
//!
//! # Console demo
//! cargo run --manifest-path examples/streaming_bash/Cargo.toml -- cli
//! ```
//!
//! Requires: `GOOGLE_API_KEY`

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::identity::{SessionId, UserId};
use adk_core::{Agent, Content, Llm, Part};
use adk_devtools::{BashTool, Workspace};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

mod server;

const APP_NAME: &str = "streaming-bash";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| tracing_subscriber::EnvFilter::new("warn,streaming_bash_example=info,server=info"),
        ))
        .with_target(false)
        .without_time()
        .init();

    // Default: launch the web UI. Pass `cli` to run the console demo instead.
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "cli" {
        return run_cli().await;
    }

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3000);
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Streaming Bash — Live Terminal Output in the Browser         ║");
    println!("║                                                              ║");
    println!("║  BashTool.emit_progress() chunks ride the SAME EventStream    ║");
    println!("║  as the model's reply. The server relays both over one WS;    ║");
    println!("║  the browser renders chat + a live terminal from one stream.  ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    server::run_server(port).await
}

/// The original console demo (run with `cargo run -- cli`).
async fn run_cli() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Streaming Bash — Live Terminal Output from LlmAgent          ║");
    println!("║                                                              ║");
    println!("║  BashTool calls ctx.emit_progress() for each line of output. ║");
    println!("║  The framework forwards each chunk as a partial Event, so    ║");
    println!("║  this UI prints live terminal output on the SAME stream it    ║");
    println!("║  reads the model's reply from — no log scraping required.    ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    let workspace = Workspace::new(std::env::current_dir()?).allow_bash(true);
    // Bash-only: `bash` is the one DevTool that streams via emit_progress, so the
    // agent gets only it — every tool call then produces live terminal output.
    let bash = Arc::new(BashTool::new(workspace));

    let agent = LlmAgentBuilder::new("bash-demo")
        .model(model)
        .instruction(
            "You are a developer assistant with exactly one tool: `bash`. Accomplish \
             every request by running a shell command (ls, cat, head, find, uname, …). \
             Keep explanations to one sentence before running a command. \
             After running, summarize the result briefly.",
        )
        .tool(bash)
        .build()?;

    let agent: Arc<dyn Agent> = Arc::new(agent);
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

    let runner = Runner::builder()
        .app_name(APP_NAME)
        .agent(agent)
        .session_service(sessions.clone())
        .build()?;

    // ─── Demo 1: Simple command ──────────────────────────────────────────────
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Demo 1: List current directory");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  👤 List the Cargo.toml files in this directory\n");

    run_turn(
        &runner,
        &sessions,
        "s1",
        "List any Cargo.toml files in the current directory using ls",
    )
    .await?;

    // ─── Demo 2: Multi-line output ───────────────────────────────────────────
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Demo 2: System info");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  👤 Show OS, date, and working directory\n");

    run_turn(
        &runner,
        &sessions,
        "s2",
        "Show the OS name (uname), current date, and working directory — one bash command",
    )
    .await?;

    // ─── Demo 3: stderr ──────────────────────────────────────────────────────
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Demo 3: Command with stderr");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  👤 List /nonexistent_path_xyz123\n");

    run_turn(
        &runner,
        &sessions,
        "s3",
        "Try to list /nonexistent_path_xyz123 and tell me what happened",
    )
    .await?;

    // ─── Summary ─────────────────────────────────────────────────────────────
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  How emit_progress Works");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("  1. BashTool calls ctx.emit_progress(\"stdout\", line) per line.");
    println!("  2. The framework forwards each chunk as a partial Event tagged");
    println!("     with a stream label, interleaved on the agent's EventStream.");
    println!("  3. The UI reads ONE stream: model text and live terminal output");
    println!("     arrive together, distinguished by event.tool_progress_stream().");
    println!();
    println!("  Web UIs:  relay progress events over WebSocket to a terminal widget.");
    println!("  IDEs:     pipe them to an embedded terminal panel.");
    println!("  CLIs:     print directly, as shown above.");

    Ok(())
}

async fn run_turn(
    runner: &Runner,
    sessions: &Arc<dyn SessionService>,
    session_id: &str,
    prompt: &str,
) -> anyhow::Result<()> {
    // Create session if it doesn't exist
    let _ = sessions
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: "user".to_string(),
            session_id: Some(session_id.to_string()),
            state: Default::default(),
        })
        .await;

    let content = Content::new("user").with_text(prompt);
    let mut stream = runner.run(UserId::new("user")?, SessionId::new(session_id)?, content).await?;

    let mut in_terminal_block = false;
    let mut model_started = false;
    while let Some(event) = stream.next().await {
        let event = event?;

        // Tool-progress events arrive on the SAME stream as model output.
        // They carry a stream label ("stdout"/"stderr") in provider_metadata.
        if let Some(stream_name) = event.tool_progress_stream() {
            if !in_terminal_block {
                println!("\n  ┌─ live terminal ─────────────────────────────");
                in_terminal_block = true;
            }
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        let tag = if stream_name == "stderr" { "2" } else { "1" };
                        for line in text.lines() {
                            println!("  │ [{tag}] {line}");
                        }
                    }
                }
            }
            continue;
        }

        // Regular model output (the assistant's reply).
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    if in_terminal_block {
                        println!("  └─────────────────────────────────────────────");
                        in_terminal_block = false;
                    }
                    if !model_started {
                        print!("\n  🤖 ");
                        model_started = true;
                    }
                    print!("{text}");
                }
            }
        }
    }
    if in_terminal_block {
        println!("  └─────────────────────────────────────────────");
    }
    println!();

    Ok(())
}
