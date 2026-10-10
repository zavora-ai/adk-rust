//! # Autonomous goal loop ("/goal") with durable checkpointing
//!
//! Mirrors Codex/Hermes goal mode: set a goal + a **verifiable success
//! condition** (a shell command that must exit 0), and the agent loops
//! **plan → act → verify**, self-correcting from the check's output, until the
//! condition passes or the iteration budget is hit.
//!
//! Durability: after **every** iteration the goal state is atomically
//! checkpointed to disk, so a crash/restart can `--resume` from where it left
//! off (and a completed goal is recognized as done).
//!
//! This demo:
//! 1. seeds a workspace with a buggy module + a failing test,
//! 2. runs the goal loop until `python3 test_stats.py` passes,
//! 3. prints the persisted checkpoint,
//! 4. re-runs with `resume = true` to show a completed goal is a no-op,
//! 5. independently verifies the result.
//!
//! Requires `GOOGLE_API_KEY` (Gemini 3) — or `CODING_PROVIDER=openai` + `OPENAI_API_KEY`.

use std::path::Path;
use std::sync::Arc;

use adk_agent::coding::CodingAgent;
use adk_core::{Content, Llm, Part, SessionId, UserId};
use adk_devtools::Workspace;
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

const APP: &str = "coding-goal";

/// Durable goal state, checkpointed after every iteration.
#[derive(Serialize, Deserialize, Default)]
struct GoalState {
    goal: String,
    until: String,
    iteration: u32,
    /// "running" | "done" | "exhausted".
    status: String,
    last_output: String,
}

struct GoalOutcome {
    success: bool,
    iterations: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let model = build_model()?;
    let dir = tempfile::tempdir()?;
    let root = dir.path();

    // Fixture: mean() forgets to divide by len; the test expects the average.
    std::fs::write(root.join("stats.py"), "def mean(xs):\n    return sum(xs)\n")?;
    std::fs::write(
        root.join("test_stats.py"),
        "from stats import mean\n\
         assert mean([2, 4, 6]) == 4, mean([2, 4, 6])\n\
         assert mean([10, 20]) == 15, mean([10, 20])\n\
         print('ok')\n",
    )?;

    let goal = "Fix stats.py so the tests pass. Do not modify the test file.";
    let until = "python3 test_stats.py";
    let state_path = root.join(".adk").join("goal.json");

    println!("ADK-Rust — autonomous /goal loop (durable)");
    println!("workspace: {}", root.display());
    println!("goal:  {goal}");
    println!("until: {until}\n");

    // 1–2: run the goal loop to completion.
    let outcome = run_goal_loop(model.clone(), root, goal, until, 5, &state_path, false).await?;

    // 3: show the persisted checkpoint.
    println!("\n══ durable checkpoint ({}) ══", state_path.display());
    println!("{}", std::fs::read_to_string(&state_path).unwrap_or_default());

    // 4: resume a completed goal — should be a no-op.
    println!("══ resume (simulating a restart) ══");
    run_goal_loop(model, root, goal, until, 5, &state_path, true).await?;

    // 5: independent verification.
    println!("\n══ verification ══");
    let (code, out) = run_check(root, until);
    let ok = code == Some(0) && out.contains("ok");
    println!(
        "  {} {until} (exit {code:?}) after {} iteration(s)",
        if ok && outcome.success { "✅ PASS" } else { "❌ FAIL" },
        outcome.iterations
    );
    if !(ok && outcome.success) {
        std::process::exit(1);
    }
    Ok(())
}

/// The durable goal loop. Returns the outcome; checkpoints after each iteration.
async fn run_goal_loop(
    model: Arc<dyn Llm>,
    root: &Path,
    goal: &str,
    until: &str,
    max_iters: u32,
    state_path: &Path,
    resume: bool,
) -> anyhow::Result<GoalOutcome> {
    // Resume from disk if asked and a run exists.
    let mut gs = if resume {
        match std::fs::read_to_string(state_path)
            .ok()
            .and_then(|s| serde_json::from_str::<GoalState>(&s).ok())
        {
            Some(s) if s.status == "done" => {
                println!("  goal already complete (per checkpoint); nothing to do.");
                return Ok(GoalOutcome { success: true, iterations: s.iteration });
            }
            Some(s) => {
                println!("  resuming from iteration {}", s.iteration);
                s
            }
            None => fresh(goal, until),
        }
    } else {
        fresh(goal, until)
    };

    let coding = CodingAgent::builder()
        .model(model)
        .workspace(Workspace::new(root).allow_bash(true))
        .build()?;
    let runner = make_runner(coding.agent()).await?;

    let start = gs.iteration + 1;
    for iter in start..=max_iters {
        println!("━━ iteration {iter}/{max_iters} ━━");
        let prompt = if iter == 1 {
            format!(
                "Goal: {goal}\nWhen the goal is met, `{until}` must exit 0. Work toward that now."
            )
        } else {
            format!(
                "`{until}` is still failing. Latest output:\n---\n{}\n---\nFix it; goal: {goal}",
                gs.last_output
            )
        };
        run_turn(&runner, &prompt).await?;

        let (code, output) = run_check(root, until);
        gs.iteration = iter;
        gs.last_output = first_lines(&output, 30);
        gs.status = if code == Some(0) { "done".into() } else { "running".into() };
        checkpoint(state_path, &gs); // durable: persist after every iteration

        if code == Some(0) {
            println!("  ✅ goal met after {iter} iteration(s)\n");
            return Ok(GoalOutcome { success: true, iterations: iter });
        }
        println!("  ✗ `{until}` exited {code:?}; iterating\n");
    }
    gs.status = "exhausted".into();
    checkpoint(state_path, &gs);
    Ok(GoalOutcome { success: false, iterations: max_iters })
}

fn fresh(goal: &str, until: &str) -> GoalState {
    GoalState {
        goal: goal.into(),
        until: until.into(),
        status: "running".into(),
        ..Default::default()
    }
}

/// Atomically write the checkpoint (temp + rename).
fn checkpoint(path: &Path, state: &GoalState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

fn run_check(dir: &Path, command: &str) -> (Option<i32>, String) {
    match std::process::Command::new("sh").arg("-c").arg(command).current_dir(dir).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code(), s)
        }
        Err(e) => (None, e.to_string()),
    }
}

fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join("\n")
}

async fn make_runner(agent: Arc<dyn adk_core::Agent>) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "user".into(),
            session_id: Some("goal".into()),
            state: Default::default(),
        })
        .await?;
    Ok(Runner::builder().app_name(APP).agent(agent).session_service(sessions).build()?)
}

async fn run_turn(runner: &Runner, prompt: &str) -> anyhow::Result<()> {
    let mut stream = runner
        .run(UserId::new("user")?, SessionId::new("goal")?, Content::new("user").with_text(prompt))
        .await?;
    let mut pending = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, .. } => {
                        flush(&mut pending);
                        println!("  🔧 {name}");
                    }
                    Part::Text { text } if !text.is_empty() => pending.push_str(text),
                    _ => {}
                }
            }
        }
    }
    flush(&mut pending);
    Ok(())
}

fn flush(pending: &mut String) {
    let t = pending.trim();
    if !t.is_empty() {
        println!("  🤖 {t}");
    }
    pending.clear();
}

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
