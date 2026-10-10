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

mod scenarios;

use std::sync::Arc;

use adk_agent::coding::CodingAgent;
use adk_core::{Agent, Content, Llm, Part, SessionId, UserId};
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
        [one] if one == "multiturn" || one == "build" => multiturn().await,
        [one] if one == "tour" => tour().await,
        [one] if scenarios::find(one).is_some() => run_named_scenario(one).await,
        [dir, task] => single(dir, task).await,
        _ => {
            eprintln!(
                "usage:\n  coding_agent                 # multi-language demo\n  \
                 coding_agent multiturn       # build a medium program over several turns\n  \
                 coding_agent tour            # run all scenarios (increasing complexity)\n  \
                 coding_agent <scenario>      # one of: {}\n  \
                 coding_agent <dir> <task>    # a single task in a directory",
                scenarios::all().iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
            );
            std::process::exit(2);
        }
    }
}

/// Run every scenario in order, verifying each, and print a summary.
async fn tour() -> anyhow::Result<()> {
    let model = build_model()?;
    println!("ADK-Rust CodingAgent — scenario tour (increasing complexity)\n");

    let mut results: Vec<(&str, bool)> = Vec::new();
    for sc in scenarios::all() {
        let passed = run_scenario(model.clone(), &sc).await?;
        results.push((sc.name, passed));
    }

    println!("\n══ summary ══");
    for (name, passed) in &results {
        println!("  {} {name}", if *passed { "✅ PASS" } else { "❌ FAIL" });
    }
    let failed = results.iter().filter(|(_, p)| !*p).count();
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Run a single named scenario.
async fn run_named_scenario(name: &str) -> anyhow::Result<()> {
    let model = build_model()?;
    let sc = scenarios::find(name).expect("scenario exists");
    let passed = run_scenario(model, &sc).await?;
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}

/// Set up a scenario's fixture, run the agent, then verify independently.
async fn run_scenario(model: Arc<dyn Llm>, sc: &scenarios::Scenario) -> anyhow::Result<bool> {
    let dir = tempfile::tempdir()?;
    (sc.setup)(dir.path())?;

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  {} — {}", sc.name, sc.blurb);
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    run_task(model, Workspace::new(dir.path()).allow_bash(true), sc.name, sc.task).await?;

    let (passed, detail) = (sc.verify)(dir.path());
    println!("  {} verify: {detail}", if passed { "✅" } else { "❌" });
    println!();
    Ok(passed)
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

/// Build a Runner over the agent with a fresh in-memory session.
async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    Ok(Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?)
}

/// Run one turn on an existing runner/session and stream the agent's work.
/// Reusing the same `session_id` across turns preserves conversation history.
async fn run_turn(runner: &Runner, session_id: &str, prompt: &str) -> anyhow::Result<()> {
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
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
    Ok(())
}

/// Print the agent's current plan (todo list), if any.
fn print_plan(coding: &CodingAgent) {
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
}

/// Run one task against a workspace directory in a fresh session.
async fn run_task(
    model: Arc<dyn Llm>,
    workspace: Workspace,
    session_id: &str,
    task: &str,
) -> anyhow::Result<()> {
    let coding = CodingAgent::builder().model(model).workspace(workspace).build()?;
    let runner = make_runner(coding.agent(), session_id).await?;
    run_turn(&runner, session_id, task).await?;
    print_plan(&coding);
    Ok(())
}

/// Multi-language demo: one temp workspace, three tasks.
async fn demo() -> anyhow::Result<()> {
    let model = build_model()?;
    let dir = tempfile::tempdir()?;
    let workspace = Workspace::new(dir.path()).allow_bash(true);

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

/// Multi-turn build: one persistent session in which the agent grows a
/// medium-sized program (a Python `todo` CLI) over several turns, then we
/// independently verify the result by exercising the CLI and running its tests.
async fn multiturn() -> anyhow::Result<()> {
    let model = build_model()?;
    let dir = tempfile::tempdir()?;
    let session_id = "build";

    // ONE agent, ONE runner, ONE session — history persists across turns.
    let coding = CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new(dir.path()).allow_bash(true))
        .build()?;
    let runner = make_runner(coding.agent(), session_id).await?;

    println!("ADK-Rust CodingAgent — multi-turn build (a Python todo CLI)");
    println!("workspace: {}\n", dir.path().display());

    let turns: &[&str] = &[
        "Start a command-line todo app in Python in a single file `todo.py`. \
         Persist tasks as JSON in `todos.json` next to the script. Support two commands: \
         `python3 todo.py add <text>` (append a task) and `python3 todo.py list` \
         (print tasks numbered from 1). Then demonstrate it: add 'buy milk' and run list.",
        "Add a `done <index>` command that marks the task at that 1-based index complete. \
         In `list`, show completed tasks with a leading '[x] ' and pending tasks with '[ ] '. \
         Demonstrate by marking task 1 done and listing.",
        "Add a `rm <index>` command that removes the task at that 1-based index. \
         Demonstrate by adding a second task, removing the first, and listing.",
        "Make it robust: if the command is missing/unknown or the index is out of range or \
         not a number, print a helpful usage/error message and exit with a non-zero status \
         instead of crashing. Keep the existing commands working.",
        "Write `test_todo.py` that exercises add, list, done, and rm by running todo.py as a \
         subprocess against a temporary data file (set the data path via an env var like \
         TODO_FILE, adding support for it in todo.py if needed). Assert the observable \
         behavior. Run `python3 test_todo.py` and fix anything until it passes.",
    ];

    for (i, prompt) in turns.iter().enumerate() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("  turn {} / {}", i + 1, turns.len());
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("  👤 {prompt}");
        run_turn(&runner, session_id, prompt).await?;
        println!();
    }
    print_plan(&coding);

    // ── Independent verification: the program the agent built must actually work.
    println!("\n══ verification ══");
    let files: Vec<String> = std::fs::read_dir(dir.path())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    println!("  files: {}", files.join(", "));

    let loc: usize = ["todo.py", "test_todo.py"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(dir.path().join(f)).ok())
        .map(|c| c.lines().count())
        .sum();
    println!("  lines of code (todo.py + test_todo.py): {loc}");

    let test =
        std::process::Command::new("python3").arg("test_todo.py").current_dir(dir.path()).output();
    match test {
        Ok(o) if o.status.success() => {
            println!("  ✅ python3 test_todo.py passed");
            Ok(())
        }
        Ok(o) => {
            println!(
                "  ❌ python3 test_todo.py failed (exit {:?})\n{}",
                o.status.code(),
                String::from_utf8_lossy(&o.stderr).trim()
            );
            std::process::exit(1);
        }
        Err(e) => {
            println!("  ❌ could not run tests: {e}");
            std::process::exit(1);
        }
    }
}

/// Run a single task in a user-supplied directory.
async fn single(dir: &str, task: &str) -> anyhow::Result<()> {
    let model = build_model()?;
    let workspace = Workspace::new(dir).allow_bash(true);
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
