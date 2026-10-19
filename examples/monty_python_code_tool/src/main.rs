//! # MontyPythonCodeTool example — REPL-mode Python for an `LlmAgent`
//!
//! Runs an [`LlmAgent`] with a REPL-mode [`MontyPythonCodeTool`]: the model writes
//! Python that executes **in-process** via the Monty interpreter — no
//! container, no subprocess. The tool is configured with:
//!
//! 1. a **read-write mount** (`/out` → a temp directory on the host), so
//!    scripts can persist files through `pathlib.Path`;
//! 2. an **environment variable** (`PROJECT`), readable with `os.getenv`;
//! 3. a **registered host function** (`fx_rate`), callable from Python by
//!    bare name — a Rust async function the interpreter suspends into.
//!
//! Because the tool is in REPL mode, variables persist across tool calls
//! within the session: turn 2 reuses state computed in turn 1. The tool's
//! LLM-facing description is composed from the executor's own capability
//! report, so the model knows exactly which paths, variables, and functions
//! exist.
//!
//! ## Run
//!
//! ```bash
//! GOOGLE_API_KEY=... cargo run --manifest-path examples/monty_python_code_tool/Cargo.toml
//! ```

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_code::PathAccess;
use adk_core::{Agent, Content, Part, SessionId, Tool, UserId};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::MontyPythonCodeTool;
use futures::StreamExt;
use serde_json::{Value, json};

const APP_NAME: &str = "monty-python-code-tool-example";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    println!("=== ADK-Rust MontyPythonCodeTool example ===\n");

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set in .env or environment");
    let model = GeminiModel::new(api_key, "gemini-3.5-flash-lite")?;

    // The host-authored trust boundary: one writable mount, one environment
    // variable, the clock, and one host function. Everything else — other
    // paths, other variables, network, subprocesses — is unreachable from
    // Python regardless of what the model writes.
    let out_dir = tempfile::tempdir()?;
    let tool = MontyPythonCodeTool::builder()
        .allow_path("/out", out_dir.path(), PathAccess::ReadWrite)
        .environ_var("PROJECT", "acme")
        .system_clock()
        .function_fn(
            "fx_rate",
            "Exchange rate from one currency code to another, e.g. fx_rate('EUR', 'USD').",
            |args, _kwargs| async move {
                let pair = (
                    args.first().and_then(Value::as_str).unwrap_or_default(),
                    args.get(1).and_then(Value::as_str).unwrap_or_default(),
                );
                match pair {
                    ("EUR", "USD") => Ok(json!(1.09)),
                    ("USD", "EUR") => Ok(json!(0.92)),
                    (from, to) => Err(format!("no rate for {from}->{to}; try EUR/USD").into()),
                }
            },
        )
        .build_repl()?;

    // The description is composed from the executor's own capability report.
    println!("--- Tool description handed to the model ---\n{}\n", tool.description());

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("python_analyst")
            .description("Answers data questions by writing Python")
            .instruction(
                "Use the monty_python_code tool for every calculation. In REPL mode variables \
                 persist between calls, so reuse what you already computed. Report results \
                 concisely.",
            )
            .model(Arc::new(model))
            .tool(Arc::new(tool))
            .build()?,
    );

    let session_id = "session-1";
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    // Turn 1: compute in EUR, convert via the host function, write a file to
    // the granted mount, and keep the totals in Python variables.
    run_turn(
        &runner,
        session_id,
        "Q3 invoices in EUR: 1200.50, 940.00, 3310.75. Store them in a variable, compute \
         the total in EUR and in USD (use fx_rate), and write a one-line summary to \
         /out/q3.txt. Tag it with the PROJECT environment variable.",
    )
    .await?;

    // Turn 2: the REPL session still holds the variables from turn 1.
    run_turn(
        &runner,
        session_id,
        "Using the invoice data you already have, what is the average invoice in USD?",
    )
    .await?;

    // The write landed on the real host directory behind the mount.
    let written = std::fs::read_to_string(out_dir.path().join("q3.txt"))?;
    println!("--- Host file written by the model ---\n{written}\n");

    println!("Done.");
    Ok(())
}

/// Send one user message and print the agent's streamed replies.
async fn run_turn(runner: &Runner, session_id: &str, prompt: &str) -> anyhow::Result<()> {
    println!(">>> {prompt}\n");
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                println!("[{}]\n{text}\n", event.author);
            }
        }
    }
    Ok(())
}
