#[cfg(feature = "audit")]
mod audit;
mod cli;
mod deploy;
mod graph;
#[cfg(feature = "vertex-agent-registry")]
mod registry;
#[cfg(feature = "scripted-model")]
mod scripted;
#[cfg(feature = "persist")]
mod sessions;
mod setup;
mod skills;
mod ultra;

use adk_agent::LlmAgentBuilder;
use adk_agent::coding::CodingAgent;
use adk_cli::{Launcher, launcher::ThinkingDisplayMode};
use adk_core::{Content, Llm, Part, SessionId, UserId};
use adk_devtools::Workspace;
use adk_model::ModelProvider;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::GoogleSearchTool;
use anyhow::Result;
use clap::Parser;
use cli::{AgentOptions, ChatArgs, Cli, Commands, ThinkingMode};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let Cli { command, options } = Cli::parse();

    match command {
        None => run_chat(&options, ChatArgs::default()).await,
        Some(Commands::Chat(args)) => run_chat(&options, args).await,
        Some(Commands::Serve { port, host }) => {
            let agent = build_agent(&options)?;
            Launcher::new(Arc::new(agent))
                .app_name("adk-rust")
                .with_bind_address(host)
                .run_serve_directly(port)
                .await
                .map_err(Into::into)
        }
        Some(Commands::Code { task, dir, read_only }) => {
            let model = resolve_model(&options)?;
            run_code(model, options.instruction, dir, read_only, task).await
        }
        Some(Commands::Goal { goal, until, dir, max_iters, state, resume }) => {
            let model = resolve_model(&options)?;
            run_goal(GoalArgs {
                model,
                instruction: options.instruction,
                dir,
                goal,
                until,
                max_iters,
                state,
                resume,
            })
            .await
        }
        Some(Commands::Ultracode { task, dir, max_rounds }) => {
            let model = resolve_model(&options)?;
            ultra::run(model.llm, &model.id, options.instruction, dir, task, max_rounds).await
        }
        Some(Commands::Skills { command }) => skills::run(command).await,
        Some(Commands::Deploy { command }) => deploy::run(command).await,
        #[cfg(feature = "vertex-agent-registry")]
        Some(Commands::Registry { command }) => registry::run(command).await,
        Some(Commands::Graph { command }) => graph::run(command).await,
        #[cfg(feature = "audit")]
        Some(Commands::Audit { command }) => audit::run(command).await,
    }
}

/// The model an agent command runs on, resolved through [`setup::resolve`].
struct ResolvedModel {
    llm: Arc<dyn Llm>,
    /// Display id, derived from the flags or saved config only, never from the key.
    id: String,
    provider: Option<ModelProvider>,
    /// The chat instruction: `--instruction`, or the default one.
    instruction: String,
}

/// Resolves the model from the flags, saved config, secure storage, or first-run setup
/// (spec 3.1 §8.1).
fn resolve_model(options: &AgentOptions) -> Result<ResolvedModel> {
    #[cfg(feature = "scripted-model")]
    if let Some(script) = &options.model_script {
        return Ok(ResolvedModel {
            llm: scripted::model(script)?,
            id: "scripted".to_string(),
            provider: None,
            instruction: options.instruction.clone().unwrap_or_else(setup::default_instruction),
        });
    }
    let resolved = setup::resolve(
        options.provider,
        options.model.clone(),
        options.api_key.clone(),
        options.instruction.clone(),
    )?;
    let llm = create_model(
        resolved.provider,
        &resolved.model,
        resolved.api_key.as_deref(),
        options.thinking_budget,
    )?;
    Ok(ResolvedModel {
        llm,
        id: resolved.model,
        provider: Some(resolved.provider),
        instruction: resolved.instruction,
    })
}

/// The interactive console, with its conversation kept per [`ChatArgs`].
async fn run_chat(options: &AgentOptions, args: ChatArgs) -> Result<()> {
    let agent = build_agent(options)?;
    let launcher = Launcher::new(Arc::new(agent))
        .app_name("adk-rust")
        .with_thinking_mode(map_thinking_mode(options.thinking_mode));
    with_chat_sessions(launcher, args).await?.run_console_directly().await.map_err(Into::into)
}

/// Keeps the conversation in `harness.db` unless `--ephemeral` (spec 3.1 §8.1).
#[cfg(feature = "persist")]
async fn with_chat_sessions(launcher: Launcher, args: ChatArgs) -> Result<Launcher> {
    if args.ephemeral {
        return Ok(launcher);
    }
    let store = sessions::open().await?;
    let user_id = sessions::os_user();
    let session_id = match args.session {
        Some(session_id) => Some(session_id),
        None if args.continue_session => {
            let latest = sessions::latest(store.as_ref(), &user_id).await?;
            if latest.is_none() {
                println!("No earlier session to continue; starting a new one.");
            }
            latest
        }
        None => None,
    };
    let launcher = launcher.with_session_service(store).with_user_id(user_id);
    Ok(match session_id {
        Some(session_id) => launcher.with_session_id(session_id),
        None => launcher,
    })
}

#[cfg(not(feature = "persist"))]
async fn with_chat_sessions(launcher: Launcher, args: ChatArgs) -> Result<Launcher> {
    if args.session.is_some() || args.continue_session {
        anyhow::bail!(
            "persistent sessions are not compiled into this adk-cli build. Reinstall with \
             `--features persist`, or start without --session and --continue."
        );
    }
    Ok(launcher)
}

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
struct GoalArgs {
    model: ResolvedModel,
    instruction: Option<String>,
    dir: String,
    goal: String,
    until: String,
    max_iters: u32,
    state: Option<String>,
    resume: bool,
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
async fn run_goal(args: GoalArgs) -> Result<()> {
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
async fn run_code(
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

fn build_agent(options: &AgentOptions) -> Result<adk_agent::LlmAgent> {
    let model = resolve_model(options)?;

    let mut builder = LlmAgentBuilder::new("adk_agent")
        .description("Default ADK-Rust CLI agent")
        .instruction(model.instruction)
        .model(model.llm);

    // Google Search grounding only works with Gemini
    if model.provider == Some(ModelProvider::Gemini) {
        builder = builder.tool(Arc::new(GoogleSearchTool::new()));
    }

    #[cfg(feature = "scripted-model")]
    if options.approval_fixture {
        builder = builder
            .tool(Arc::new(scripted::RecordNote))
            .require_tool_confirmation(scripted::RecordNote::NAME);
    }

    builder.build().map_err(Into::into)
}

fn create_model(
    provider: ModelProvider,
    model: &str,
    api_key: Option<&str>,
    thinking_budget: Option<u32>,
) -> Result<Arc<dyn Llm>> {
    match provider {
        #[cfg(feature = "gemini")]
        ModelProvider::Gemini => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Gemini requires an API key"))?;
            let m = adk_model::GeminiModel::new(key, model)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "gemini"))]
        ModelProvider::Gemini => Err(setup::not_compiled(provider)),
        #[cfg(feature = "openai")]
        ModelProvider::Openai => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("OpenAI requires an API key"))?;
            let config = adk_model::OpenAIConfig::new(key, model);
            let m = adk_model::OpenAIClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "openai"))]
        ModelProvider::Openai => Err(setup::not_compiled(provider)),
        #[cfg(feature = "anthropic")]
        ModelProvider::Anthropic => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Anthropic requires an API key"))?;
            let mut config = adk_model::anthropic::AnthropicConfig::new(key, model);
            if let Some(budget) = thinking_budget {
                if budget == 0 {
                    return Err(anyhow::anyhow!("--thinking-budget must be greater than 0"));
                }
                config = config.with_thinking(budget);
            }
            let m = adk_model::AnthropicClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "anthropic"))]
        ModelProvider::Anthropic => Err(setup::not_compiled(provider)),
        #[cfg(feature = "deepseek")]
        ModelProvider::Deepseek => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("DeepSeek requires an API key"))?;
            let config = adk_model::DeepSeekConfig::new(key, model);
            let m = adk_model::DeepSeekClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "deepseek"))]
        ModelProvider::Deepseek => Err(setup::not_compiled(provider)),
        #[cfg(feature = "groq")]
        ModelProvider::Groq => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Groq requires an API key"))?;
            let config = adk_model::GroqConfig::new(key, model);
            let m = adk_model::GroqClient::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "groq"))]
        ModelProvider::Groq => Err(setup::not_compiled(provider)),
        #[cfg(feature = "ollama")]
        ModelProvider::Ollama => {
            reject_unsupported_thinking_budget(provider, thinking_budget)?;
            let config = adk_model::OllamaConfig::new(model);
            let m = adk_model::OllamaModel::new(config)?;
            Ok(Arc::new(m))
        }
        #[cfg(not(feature = "ollama"))]
        ModelProvider::Ollama => Err(setup::not_compiled(provider)),
    }
}

fn reject_unsupported_thinking_budget(
    provider: ModelProvider,
    thinking_budget: Option<u32>,
) -> Result<()> {
    if thinking_budget.is_some() {
        Err(anyhow::anyhow!("--thinking-budget is not supported for provider {}", provider))
    } else {
        Ok(())
    }
}

fn map_thinking_mode(mode: ThinkingMode) -> ThinkingDisplayMode {
    match mode {
        ThinkingMode::Auto => ThinkingDisplayMode::Auto,
        ThinkingMode::Show => ThinkingDisplayMode::Show,
        ThinkingMode::Hide => ThinkingDisplayMode::Hide,
    }
}
