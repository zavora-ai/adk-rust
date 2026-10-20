//! `code`, `goal` and `ultracode` without the harness: the `CodingAgent` with host `bash`
//! and no approvals, kept for builds without the `harness` feature.

use std::sync::Arc;

use adk_agent::coding::CodingAgent;
use adk_core::{Content, Part, SessionId, UserId};
use adk_devtools::Workspace;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::Result;
use futures::StreamExt;

use crate::ResolvedModel;

/// Drive one agent turn on an existing runner/session, streaming the trace.
async fn stream_turn(runner: &Runner, session_id: &str, prompt: &str) -> Result<()> {
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;
    let mut pending = String::new();
    let mut text_deltas = adk_core::EventTextDeltas::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        let event = text_deltas.push(&event);
        // A streamed function call arrives in a partial chunk and again in the complete event.
        let partial = event.llm_response.partial;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::FunctionCall { name, args, .. } if !partial => {
                        flush_text(&mut pending);
                        println!("  🔧 {name}({})", first_line(&args.to_string()));
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        flush_text(&mut pending);
                        println!("  ↩  {}", first_line(&function_response.response.to_string()));
                    }
                    Part::Text { text } if !text.is_empty() => pending.push_str(text),
                    _ => {}
                }
            }
        }
    }
    flush_text(&mut pending);
    Ok(())
}

/// Run a shell command in `dir`; returns (exit_code, combined stdout+stderr).
/// Run the goal-mode success check.
///
/// Uses the same environment policy as the agent's `bash` tool, so a check cannot see
/// credentials the agent itself is not allowed to see.
fn run_check(dir: &str, command: &str) -> (Option<i32>, String) {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(dir);

    let workspace = adk_devtools::Workspace::new(dir);
    if !workspace.inherits_env() {
        cmd.env_clear();
        for (key, value) in workspace.bash_env() {
            cmd.env(key, value);
        }
    }

    match cmd.output() {
        Ok(o) => {
            let mut out = String::from_utf8_lossy(&o.stdout).to_string();
            out.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code(), out)
        }
        Err(e) => (None, e.to_string()),
    }
}

/// Arguments for [`run_goal`].
pub struct GoalArgs {
    pub model: ResolvedModel,
    pub instruction: Option<String>,
    pub dir: String,
    pub goal: String,
    pub until: String,
    pub max_iters: u32,
    pub state: Option<String>,
    pub resume: bool,
}

/// Durable goal state, checkpointed to disk after every iteration so a run can
/// resume across restarts (mirrors Codex/Hermes persistent `/goal`).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct GoalState {
    goal: String,
    until: String,
    iteration: u32,
    /// "running" | "done" | "exhausted".
    status: String,
    last_output: String,
}

/// Autonomous goal mode: loop plan → act → verify until `until` passes or budget.
pub async fn run_goal(args: GoalArgs) -> Result<()> {
    let GoalArgs { model, instruction, dir, goal, until, max_iters, state, resume } = args;

    let model_id = model.id;
    let mut coding =
        CodingAgent::builder().model(model.llm).workspace(Workspace::new(&dir).allow_bash(true));
    if let Some(instruction) = instruction {
        coding = coding.instruction(instruction);
    }
    let coding = coding.build()?;

    let state_path = state.unwrap_or_else(|| format!("{dir}/.adk/goal.json"));

    // Resume: if a saved state exists and is still running, continue from it.
    let mut gs = if resume {
        match std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str::<GoalState>(&s).ok())
        {
            Some(saved) if saved.status == "done" => {
                println!("goal already complete (per {state_path}); nothing to do.");
                return Ok(());
            }
            Some(saved) => {
                println!("resuming goal from {state_path} (iteration {})", saved.iteration);
                saved
            }
            None => {
                println!("no resumable state at {state_path}; starting fresh.");
                GoalState {
                    goal: goal.clone(),
                    until: until.clone(),
                    status: "running".into(),
                    ..Default::default()
                }
            }
        }
    } else {
        GoalState {
            goal: goal.clone(),
            until: until.clone(),
            status: "running".into(),
            ..Default::default()
        }
    };

    // One runner/session: the agent remembers prior attempts across iterations.
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "adk-rust".into(),
            user_id: "user".into(),
            session_id: Some("goal".into()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("adk-rust")
        .agent(coding.agent())
        .session_service(sessions)
        .build()?;

    println!("goal mode ({model_id}) on {dir}");
    println!("goal:  {goal}");
    println!("until: {until}  (budget: {max_iters} iterations, state: {state_path})\n");

    let start = gs.iteration + 1;
    for iter in start..=max_iters {
        println!("━━ iteration {iter}/{max_iters} ━━");
        let prompt = if iter == 1 {
            format!(
                "Goal: {goal}\n\nWhen you believe the goal is met, the command `{until}` must \
                 succeed (exit 0). Work toward that now."
            )
        } else {
            format!(
                "The success check `{until}` is not passing yet. Its latest output was:\n\
                 ---\n{}\n---\nDiagnose and fix this, continuing toward the goal: {goal}",
                gs.last_output
            )
        };
        stream_turn(&runner, "goal", &prompt).await?;

        let (code, output) = run_check(&dir, &until);
        gs.iteration = iter;
        gs.last_output = first_lines(&output, 40);
        gs.status = if code == Some(0) { "done".into() } else { "running".into() };
        save_goal_state(&state_path, &gs); // checkpoint after every iteration

        if code == Some(0) {
            println!(
                "\n✅ goal met after {iter} iteration(s): `{until}` passed. (state: {state_path})"
            );
            return Ok(());
        }
        println!("  ✗ check `{until}` exited {code:?}; iterating.\n");
    }
    gs.status = "exhausted".into();
    save_goal_state(&state_path, &gs);
    println!("\n⚠️  budget exhausted; `{until}` still failing. Resume later with --resume.");
    std::process::exit(1);
}

/// Atomically checkpoint goal state to disk (write temp + rename).
fn save_goal_state(path: &str, state: &GoalState) {
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let tmp = format!("{path}.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// First `n` lines of `s` (for compact failure feedback).
fn first_lines(s: &str, n: usize) -> String {
    s.lines().take(n).collect::<Vec<_>>().join("\n")
}

/// Run the coding agent on a single task in a workspace directory.
pub async fn run_code(
    model: ResolvedModel,
    instruction: Option<String>,
    dir: String,
    read_only: bool,
    task: String,
) -> Result<()> {
    let model_id = model.id;
    let workspace =
        if read_only { Workspace::read_only(&dir) } else { Workspace::new(&dir).allow_bash(true) };
    let mut coding = CodingAgent::builder().model(model.llm).workspace(workspace);
    if let Some(instruction) = instruction {
        coding = coding.instruction(instruction);
    }
    let coding = coding.build()?;

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "adk-rust".into(),
            user_id: "user".into(),
            session_id: Some("code".into()),
            state: Default::default(),
        })
        .await?;
    let runner = Runner::builder()
        .app_name("adk-rust")
        .agent(coding.agent())
        .session_service(sessions)
        .build()?;

    println!("coding agent ({model_id}) on {dir}\ntask: {task}\n");
    stream_turn(&runner, "code", &task).await?;

    let todos = coding.todos();
    if !todos.is_empty() {
        println!("\nplan:");
        for t in todos {
            let mark = match t.status.as_str() {
                "completed" => "✓",
                "in_progress" => "▶",
                _ => "·",
            };
            println!("  {mark} {}", t.content);
        }
    }
    Ok(())
}

fn flush_text(pending: &mut String) {
    let trimmed = pending.trim();
    if !trimmed.is_empty() {
        println!("  🤖 {trimmed}");
    }
    pending.clear();
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 160 { format!("{}…", &line[..160]) } else { line.to_string() }
}
