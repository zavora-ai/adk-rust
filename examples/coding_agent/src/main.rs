//! # Coding Agent example
//!
//! Drives the ADK-Rust [`CodingAgent`] (the `adk-devtools` toolset + the harness
//! in `adk-agent`) against real tasks. The agent reads/writes/edits files and
//! runs commands in a **sandboxed workspace**.
//!
//! ## Run
//!
//! ```bash
//! # Multi-language demo (Rust, Python, JavaScript) in a temp workspace:
//! cargo run --manifest-path examples/coding_agent/Cargo.toml
//!
//! # A single task in a directory of your choosing:
//! cargo run --manifest-path examples/coding_agent/Cargo.toml -- ./some/dir "make tests pass"
//! ```
//!
//! Requires `GOOGLE_API_KEY` (default, Gemini) — or set `CODING_PROVIDER=openai`
//! with `OPENAI_API_KEY`. Override the model with `CODING_MODEL`.

use std::sync::Arc;

use adk_agent::coding::CodingAgent;
use adk_core::{Content, Llm, Part, SessionId, UserId};
use adk_devtools::Workspace;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;

const APP_NAME: &str = "coding-agent-example";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => demo().await,
        [dir, task] => single(dir, task).await,
        _ => {
            eprintln!("usage: coding_agent [<dir> <task>]   (no args = multi-language demo)");
            std::process::exit(2);
        }
    }
}

/// Build the configured model from the environment.
fn build_model() -> anyhow::Result<Arc<dyn Llm>> {
    let provider = std::env::var("CODING_PROVIDER").unwrap_or_else(|_| "gemini".into());
    match provider.as_str() {
        "openai" => {
            use adk_model::openai::{OpenAIClient, OpenAIConfig};
            let key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
            let model = std::env::var("CODING_MODEL").unwrap_or_else(|_| "gpt-5-mini".into());
            Ok(Arc::new(OpenAIClient::new(OpenAIConfig::new(key, model))?))
        }
        _ => {
            let key = std::env::var("GOOGLE_API_KEY")
                .or_else(|_| std::env::var("GEMINI_API_KEY"))
                .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY / GEMINI_API_KEY is not set"))?;
            let model =
                std::env::var("CODING_MODEL").unwrap_or_else(|_| "gemini-3.1-flash-lite".into());
            Ok(Arc::new(GeminiModel::new(&key, &model)?))
        }
    }
}

/// Run one task against a workspace directory and stream the agent's work.
async fn run_task(
    model: Arc<dyn Llm>,
    workspace: Workspace,
    session_id: &str,
    task: &str,
) -> anyhow::Result<()> {
    let coding = CodingAgent::builder().model(model).workspace(workspace).build()?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name(APP_NAME)
        .agent(coding.agent())
        .session_service(sessions)
        .build()?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(task),
        )
        .await?;

    let mut pending = String::new();
    let mut saw_anything = false;
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } => {
                        flush_text(&mut pending);
                        println!("  🔧 {name}({})", compact(args));
                        saw_anything = true;
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        flush_text(&mut pending);
                        println!("  ↩  {}", first_line(&function_response.response.to_string()));
                        saw_anything = true;
                    }
                    Part::Text { text } if !text.is_empty() => {
                        pending.push_str(text);
                        saw_anything = true;
                    }
                    _ => {}
                }
            }
        }
    }
    flush_text(&mut pending);
    if !saw_anything {
        println!("  ⚠️  the model returned an empty turn (no tools, no text)");
    }

    let todos = coding.todos();
    if !todos.is_empty() {
        println!("  📋 plan:");
        for t in todos {
            let mark = match t.status.as_str() {
                "completed" => "✓",
                "in_progress" => "▶",
                _ => "·",
            };
            println!("     {mark} {}", t.content);
        }
    }
    Ok(())
}

/// Multi-language demo: one temp workspace, three tasks.
async fn demo() -> anyhow::Result<()> {
    let model = build_model()?;
    let dir = tempfile::tempdir()?;
    let workspace = Workspace::new(dir.path());

    println!("ADK-Rust CodingAgent — multi-language demo");
    println!("workspace: {}\n", dir.path().display());

    let tasks: &[(&str, &str)] = &[
        (
            "Rust",
            "Create a file `add.rs` with a function `add(a: i32, b: i32) -> i32` and a `main` \
             that prints the result of add(2, 3). Then compile it with `rustc add.rs -o add` \
             and run `./add`. Tell me the output.",
        ),
        (
            "Python",
            "Create `fib.py` that prints the first 10 Fibonacci numbers on one line, \
             space-separated. Run it with `python3 fib.py` and report the output.",
        ),
        (
            "JavaScript",
            "Create `greet.js` that uses console.log to print exactly 'hello from node'. \
             Run it with `node greet.js` and confirm the output.",
        ),
    ];

    for (i, (lang, task)) in tasks.iter().enumerate() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("  {lang}");
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        run_task(model.clone(), workspace.clone(), &format!("s{i}"), task).await?;
        println!();
    }

    println!("Files produced in the workspace:");
    for entry in std::fs::read_dir(dir.path())? {
        let entry = entry?;
        println!("  - {}", entry.file_name().to_string_lossy());
    }
    Ok(())
}

/// Run a single task in a user-supplied directory.
async fn single(dir: &str, task: &str) -> anyhow::Result<()> {
    let model = build_model()?;
    let workspace = Workspace::new(dir);
    println!("CodingAgent on {dir}\ntask: {task}\n");
    run_task(model, workspace, "single", task).await
}

fn flush_text(pending: &mut String) {
    let trimmed = pending.trim();
    if !trimmed.is_empty() {
        println!("  🤖 {trimmed}");
    }
    pending.clear();
}

fn compact(v: &serde_json::Value) -> String {
    let s = v.to_string();
    first_line(&s)
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 160 { format!("{}…", &line[..160]) } else { line.to_string() }
}
