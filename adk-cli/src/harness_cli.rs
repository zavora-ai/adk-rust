//! `adk-rust` on the harness: the REPL, `code`, `goal`, `ultracode` and `serve`
//! (spec 3.1 §8.1, §8.2).

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::{
    BearerTokenExtractor, ConsolePrompter, HarnessArgs, HarnessLauncher, OPERATOR_SCOPE,
    ResolvedHarnessArgs, TurnOutcome,
};
use adk_core::{ApprovalPolicy, ApprovalService, ApproverSelector, Llm, StoreApprovalService};
use adk_devtools::Workspace;
use adk_harness::{
    ApprovalPrompter, FileWorkspaceTrustStore, GoalState, GoalStatus, HarnessAgent, HarnessMode,
    ShellIsolation,
};
use adk_session::{InMemorySessionService, SessionService};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::json;

use crate::ResolvedModel;
use crate::ultra::{self, UltraAgents};

/// Exit code of a run that left calls held for a decision (spec 3.1 §8.1).
pub const EXIT_HELD: i32 = 3;

/// Session-state key mirroring the goal's state (spec 3.1 §7.11).
const GOAL_STATE_KEY: &str = "harness:goal";

/// The user's harness config file.
fn user_config() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|dir| dir.join("adk-rust").join("harness.toml"))
}

/// Everything a harness command needs to build its agent and launcher.
struct Setup {
    model: Arc<dyn Llm>,
    model_id: String,
    instruction: Option<String>,
    args: HarnessArgs,
    resolved: ResolvedHarnessArgs,
    user: String,
    #[cfg(feature = "persist")]
    db: Option<Arc<crate::sessions::HarnessDb>>,
    /// The embedder long-term memory recalls with, and its description.
    embedder: Option<(Arc<adk_harness::RagEmbedder>, &'static str)>,
}

impl Setup {
    async fn new(
        model: ResolvedModel,
        instruction: Option<String>,
        args: HarnessArgs,
        default_mode: HarnessMode,
    ) -> Result<Self> {
        let resolved = args.resolve(default_mode, user_config().as_deref())?;
        #[cfg(feature = "persist")]
        let db = if args.ephemeral {
            None
        } else {
            Some(Arc::new(crate::sessions::open_harness_db().await?))
        };
        #[cfg(not(feature = "persist"))]
        if args.session.is_some() || args.continue_session {
            bail!(
                "persistent sessions are not compiled into this adk-cli build. Reinstall with \
                 `--features persist`, or start without --session and --continue."
            );
        }
        let embedder = if resolved.memory { memory_embedder(&model) } else { None };
        Ok(Self {
            embedder,
            model: model.llm,
            model_id: model.id,
            instruction,
            args,
            resolved,
            user: crate::setup::os_user(),
            #[cfg(feature = "persist")]
            db,
        })
    }

    fn sessions(&self) -> Arc<dyn SessionService> {
        #[cfg(feature = "persist")]
        if let Some(db) = &self.db {
            return db.sessions.clone();
        }
        Arc::new(InMemorySessionService::new())
    }

    /// The harness agent named `name` in `mode`, with the OS sandbox when `sandbox`.
    ///
    /// A served harness (`approval_service` set) applies standing rules and Auto only to
    /// authenticated callers (spec 3.1 §9.2 I6).
    fn agent(
        &self,
        name: &str,
        mode: HarnessMode,
        sandbox: bool,
        prompter: Option<Arc<dyn ApprovalPrompter>>,
        approval_service: Option<Arc<dyn ApprovalService>>,
    ) -> Result<HarnessAgent> {
        let workspace = Workspace::new(&self.resolved.workspace).allow_bash(true);
        let mut builder = HarnessAgent::builder(name)
            .model(self.model.clone())
            .workspace(workspace)
            .mode(mode)
            .confirmation_timeout(adk_cli::launcher::CONSOLE_CONFIRMATION_TIMEOUT);
        if let Ok(trust) = FileWorkspaceTrustStore::in_home() {
            builder = builder.workspace_trust(Arc::new(trust));
        }
        if let Some(instruction) = &self.instruction {
            builder = builder.instruction(instruction.clone());
        }
        #[cfg(feature = "persist")]
        if let Some(db) = &self.db {
            builder = builder
                .mode_store(db.modes.clone())
                .standing_rules(db.rules.clone())
                .shell_ledger(db.ledger.clone())
                .approval_store(db.approvals.clone());
            // Long-term memory in harness.db; off when served (spec 3.1 §7.15.2).
            if self.resolved.memory && approval_service.is_none() {
                builder = builder
                    .long_term_memory(adk_harness::LongTermMemoryConfig::default())
                    .fact_store(db.facts.clone());
                if let Some((embedder, _)) = &self.embedder {
                    builder = builder.memory_embedder(embedder.clone());
                }
            }
        }
        if sandbox {
            builder = builder.shell(ShellIsolation::OsSandbox(Default::default()));
            if self.resolved.fail_if_unavailable {
                builder = builder.fail_if_unavailable();
            }
        }
        if let Some(prompter) = prompter {
            builder = builder.prompter(prompter);
        }
        if let Some(service) = approval_service {
            builder = builder.approval_service(service).require_authenticated_identity();
        }
        for pack in &self.resolved.packs {
            builder = builder.pack(*pack);
        }
        for member in &self.resolved.members {
            builder = builder.member(member.to_spec()?);
        }
        Ok(builder.build()?)
    }

    /// The session `--session`, `--continue` or the goal lookup names, if any.
    async fn session_id(&self) -> Result<Option<String>> {
        if let Some(session) = &self.args.session {
            return Ok(Some(session.clone()));
        }
        if self.args.continue_session {
            let latest = latest_in_workspace(
                self.sessions().as_ref(),
                &self.user,
                &self.resolved.workspace,
                |_| true,
            )
            .await?;
            if latest.is_none() {
                println!("No earlier session in this workspace; starting a new one.");
            }
            return Ok(latest);
        }
        Ok(None)
    }

    /// A launcher for `agent` over this setup's sessions, user and session choice.
    async fn launcher(&self, agent: HarnessAgent) -> Result<HarnessLauncher> {
        let mut launcher = HarnessLauncher::new(agent)
            .app_name(APP_NAME)
            .with_session_service(self.sessions())
            .with_user_id(self.user.clone())
            .trust_workspace(self.resolved.trust);
        if self.resolved.mode_explicit {
            launcher = launcher.with_mode(self.resolved.mode);
        }
        if let Some(session) = self.session_id().await? {
            launcher = launcher.with_session_id(session);
        }
        for note in &self.resolved.notes {
            launcher = launcher.with_banner_note(note.clone());
        }
        Ok(launcher)
    }

    /// The prompter of a one-shot command: `--yes`, the terminal, or none, which holds
    /// calls for a decision.
    fn unattended_prompter(&self, console: &ConsoleIo) -> Option<Arc<dyn ApprovalPrompter>> {
        if self.resolved.yes {
            let prompter =
                ConsolePrompter::new(console.clone()).approve_all(self.resolved.allow_unsandboxed);
            Some(Arc::new(prompter))
        } else if console.is_terminal() {
            Some(Arc::new(ConsolePrompter::new(console.clone())))
        } else {
            None
        }
    }

    fn print_notes(&self) {
        for note in &self.resolved.notes {
            println!("note: {note}");
        }
    }

    /// Says once, on the first run with long-term memory, what is stored and where
    /// (spec 3.1 §7.15.2).
    fn memory_notice(&self) {
        if !self.memory_on() {
            return;
        }
        let Some(dir) = dirs::config_dir().map(|dir| dir.join("adk-rust")) else {
            return;
        };
        let marker = dir.join("memory-notice");
        if marker.exists() {
            return;
        }
        let recall = match &self.embedder {
            Some((_, name)) => format!("Facts are embedded with {name} so related ones are found."),
            None => "Facts are found by shared words; nothing leaves this machine for it.".into(),
        };
        println!(
            "Long-term memory is on. The harness keeps short facts from your sessions: the \
             preferences you state, how a project builds and tests, what fixed a failure. They \
             live in {} on this machine, with personal data redacted and secrets refused, and \
             are recalled in later sessions. What you said is \"known\"; what the agent \
             inferred is \"assumed\" until you confirm it. {recall}\n/memory lists them, \
             /memory forget ID removes one, `adk-rust memory export` writes them out, and \
             --no-memory turns memory off. This notice is shown once.\n",
            dir.join("harness.db").display()
        );
        let _ = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&marker, "shown\n"));
    }

    fn memory_on(&self) -> bool {
        #[cfg(feature = "persist")]
        return self.resolved.memory && self.db.is_some();
        #[cfg(not(feature = "persist"))]
        false
    }
}

/// The embedder long-term memory recalls with (spec 3.1 §7.15.2): the model's provider when
/// it embeds (Gemini, OpenAI), else Gemini or OpenAI when its key is set; with none, recall
/// matches shared words.
fn memory_embedder(model: &ResolvedModel) -> Option<(Arc<adk_harness::RagEmbedder>, &'static str)> {
    const GEMINI: &str = "Gemini (gemini-embedding-2)";
    const OPENAI: &str = "OpenAI (text-embedding-3-small)";
    let env = |names: &[&str]| {
        names.iter().find_map(|name| std::env::var(name).ok().filter(|key| !key.trim().is_empty()))
    };
    let own = match (model.provider, model.api_key.clone()) {
        (Some(adk_model::ModelProvider::Gemini), Some(key)) => Some((key, true)),
        (Some(adk_model::ModelProvider::Openai), Some(key)) => Some((key, false)),
        _ => None,
    };
    let (key, gemini) = own
        .or_else(|| env(&["GOOGLE_API_KEY", "GEMINI_API_KEY"]).map(|key| (key, true)))
        .or_else(|| env(&["OPENAI_API_KEY"]).map(|key| (key, false)))?;
    let built = if gemini {
        adk_harness::RagEmbedder::gemini(&key).map(|embedder| (embedder, GEMINI))
    } else {
        adk_harness::RagEmbedder::openai(&key).map(|embedder| (embedder, OPENAI))
    };
    built.ok().map(|(embedder, name)| (Arc::new(embedder), name))
}

const APP_NAME: &str = "adk-rust";

/// The most recently updated session of `user` in `workspace` whose state passes `keep`.
async fn latest_in_workspace(
    sessions: &dyn SessionService,
    user: &str,
    workspace: &Path,
    keep: impl Fn(&dyn adk_session::State) -> bool,
) -> Result<Option<String>> {
    let mut listed = sessions
        .list(adk_session::ListRequest {
            app_name: APP_NAME.to_string(),
            user_id: user.to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
    let recorded = workspace.display().to_string();
    for session in listed {
        let full = sessions
            .get(adk_session::GetRequest {
                app_name: APP_NAME.to_string(),
                user_id: user.to_string(),
                session_id: session.id().to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await?;
        let state = full.state();
        if state.get("harness:workspace").and_then(|value| value.as_str().map(str::to_string))
            == Some(recorded.clone())
            && keep(state)
        {
            return Ok(Some(full.id().to_string()));
        }
    }
    Ok(None)
}

/// Prints what to do about held calls and returns [`EXIT_HELD`].
fn held_exit(outcome: &TurnOutcome, persistent: bool) -> i32 {
    println!(
        "\n{} call(s) are held for a decision in session {}.",
        outcome.held.len(),
        outcome.session_id
    );
    for request in &outcome.held {
        println!("  {} {}", request.tool_name, first_line(&request.args.to_string()));
    }
    if persistent {
        println!(
            "Decide with `adk-rust approvals list` and `adk-rust approvals approve ID`, then \
             continue with `adk-rust --session {}`. Or rerun with --yes, or in a terminal.",
            outcome.session_id
        );
    } else {
        println!("Rerun in a terminal to decide them, or with --yes to approve them.");
    }
    EXIT_HELD
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let cut: String = line.chars().take(160).collect();
    if cut.len() < line.len() { format!("{cut}…") } else { cut }
}

/// `adk-rust` with no subcommand: the harness REPL (spec 3.1 §8.1).
pub async fn repl(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
) -> Result<()> {
    let setup = Arc::new(Setup::new(model, instruction, args, HarnessMode::Ask).await?);
    setup.memory_notice();
    let console = ConsoleIo::terminal()?;
    let prompter = |console: &ConsoleIo| -> Arc<dyn ApprovalPrompter> {
        let prompter = ConsolePrompter::new(console.clone());
        Arc::new(if setup.resolved.yes {
            prompter.approve_all(setup.resolved.allow_unsandboxed)
        } else {
            prompter
        })
    };
    let agent = setup.agent(
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        Some(prompter(&console)),
        None,
    )?;
    let rebuild = {
        let setup = setup.clone();
        let console = console.clone();
        Arc::new(move |sandbox: bool| {
            let prompter = ConsolePrompter::new(console.clone());
            let prompter: Arc<dyn ApprovalPrompter> = Arc::new(if setup.resolved.yes {
                prompter.approve_all(setup.resolved.allow_unsandboxed)
            } else {
                prompter
            });
            setup
                .agent(APP_NAME, setup.resolved.mode, sandbox, Some(prompter), None)
                .map_err(|error| adk_core::AdkError::config(error.to_string()))
        })
    };
    setup
        .launcher(agent)
        .await?
        .with_console(console)
        .with_rebuild(rebuild)
        .run_console_directly()
        .await
        .map_err(Into::into)
}

/// `adk-rust code`: one harness turn; [`EXIT_HELD`] when calls are held (spec 3.1 §8.1).
pub async fn code(
    model: ResolvedModel,
    instruction: Option<String>,
    mut args: HarnessArgs,
    read_only: bool,
    task: String,
) -> Result<i32> {
    if read_only {
        args.mode = Some(HarnessMode::ReadOnly);
    }
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    setup.print_notes();
    setup.memory_notice();
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let info = agent.info().clone();
    let mut launcher = setup.launcher(agent).await?.with_console(console);
    println!(
        "harness ({}) on {} · mode {} · shell {}\ntask: {task}\n",
        setup.model_id,
        setup.resolved.workspace.display(),
        setup.resolved.mode,
        info.isolation
    );
    let outcome = launcher.run_once(&task).await?;
    println!();
    // The turn is distilled into long-term memory before the process ends (spec 3.1 §7.15.2).
    launcher.close().await;
    let todos = launcher.todos().await?;
    if !todos.items.is_empty() {
        println!("todos:");
        for item in &todos.items {
            let mark = match item.status {
                adk_harness::TodoStatus::Completed => "✓",
                adk_harness::TodoStatus::InProgress => "▶",
                adk_harness::TodoStatus::Cancelled => "✗",
                adk_harness::TodoStatus::Pending => "·",
            };
            println!("  {mark} {}", adk_harness::escape_control(&item.content));
        }
        println!();
    }
    if !outcome.held.is_empty() {
        return Ok(held_exit(&outcome, persistent(&setup)));
    }
    if outcome.ended_by_denial {
        println!("The turn ended after a denial (session {}).", outcome.session_id);
    }
    println!("session: {}", outcome.session_id);
    Ok(if outcome.interrupted { 130 } else { 0 })
}

fn persistent(setup: &Setup) -> bool {
    #[cfg(feature = "persist")]
    return setup.db.is_some();
    #[cfg(not(feature = "persist"))]
    {
        let _ = setup;
        false
    }
}

/// Arguments of `adk-rust goal`.
pub struct GoalArgs {
    pub goal: String,
    pub until: String,
    pub max_iters: u32,
    pub state: Option<String>,
    pub resume: bool,
}

/// `adk-rust goal`: iterate harness turns until `until` passes (spec 3.1 §8.1, §7.11).
///
/// wave B: team-graph — `Harness::start_goal` / `Harness::goal` and the durable goal store
/// (spec 3.1 §7.11) replace this loop and its `harness:goal` mirror once the goal engine
/// lands; the session already carries the conversation and the mirror.
pub async fn goal(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    goal: GoalArgs,
) -> Result<i32> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    setup.print_notes();
    setup.memory_notice();
    if !matches!(setup.resolved.mode, HarnessMode::Ask | HarnessMode::Auto) {
        bail!(
            "a goal needs ask or auto mode, not {}: it edits files and runs commands",
            setup.resolved.mode
        );
    }
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let sessions = setup.sessions();
    let workspace = setup.resolved.workspace.clone();

    let resumed = if goal.resume && setup.args.session.is_none() {
        latest_in_workspace(sessions.as_ref(), &setup.user, &workspace, |state| {
            state
                .get(GOAL_STATE_KEY)
                .and_then(|value| serde_json::from_value::<GoalState>(value).ok())
                .is_some_and(|state| state.status != GoalStatus::Done)
        })
        .await?
    } else {
        None
    };
    let mut launcher = setup.launcher(agent).await?.with_console(console);
    if let Some(session) = &resumed {
        launcher = launcher.with_session_id(session.clone());
    }

    let until_check = |command: &str| run_check(&workspace, command);
    let mut state = GoalState {
        goal: goal.goal.clone(),
        until: goal.until.clone(),
        iteration: 0,
        max_iterations: goal.max_iters,
        status: GoalStatus::Running,
        last_check: None,
        updated_at: chrono::Utc::now(),
    };
    let mut last_output = String::new();
    if goal.resume {
        match &resumed {
            Some(session) => {
                let saved = read_goal(sessions.as_ref(), &setup.user, session).await?;
                if let Some(saved) = saved {
                    println!("resuming goal in session {session} (iteration {})", saved.iteration);
                    state = saved;
                    state.max_iterations = goal.max_iters;
                }
                let (code, output) = until_check(&state.until);
                if code == Some(0) {
                    state.status = GoalStatus::Done;
                    state.last_check = code;
                    save_goal(
                        &mut launcher,
                        sessions.as_ref(),
                        &setup.user,
                        &state,
                        goal.state.as_deref(),
                    )
                    .await?;
                    println!("goal already met: `{}` passes.", state.until);
                    return Ok(0);
                }
                last_output = first_lines(&output, 40);
            }
            None => println!("no unfinished goal in this workspace; starting fresh."),
        }
    }

    println!("goal mode ({}) on {}", setup.model_id, workspace.display());
    println!("goal:  {}", state.goal);
    println!("until: {}  (budget: {} iterations)\n", state.until, state.max_iterations);

    let start = state.iteration + 1;
    for iteration in start..=state.max_iterations {
        println!("━━ iteration {iteration}/{} ━━", state.max_iterations);
        let prompt = if iteration == 1 && last_output.is_empty() {
            format!(
                "Goal: {}\n\nWhen you believe the goal is met, the command `{}` must succeed \
                 (exit 0). Work toward that now.",
                state.goal, state.until
            )
        } else {
            format!(
                "The success check `{}` is not passing yet. Its latest output was:\n---\n{}\n---\n\
                 Diagnose and fix this, continuing toward the goal: {}",
                state.until, last_output, state.goal
            )
        };
        let outcome = launcher.run_once(&prompt).await?;
        state.iteration = iteration;
        if !outcome.held.is_empty() {
            save_goal(&mut launcher, sessions.as_ref(), &setup.user, &state, goal.state.as_deref())
                .await?;
            launcher.close().await;
            return Ok(held_exit(&outcome, persistent(&setup)));
        }
        if outcome.interrupted {
            save_goal(&mut launcher, sessions.as_ref(), &setup.user, &state, goal.state.as_deref())
                .await?;
            println!("\ninterrupted; continue with --resume.");
            launcher.close().await;
            return Ok(130);
        }
        let (code, output) = until_check(&state.until);
        state.last_check = code;
        last_output = first_lines(&output, 40);
        if code == Some(0) {
            state.status = GoalStatus::Done;
            save_goal(&mut launcher, sessions.as_ref(), &setup.user, &state, goal.state.as_deref())
                .await?;
            println!("\ngoal met after {iteration} iteration(s): `{}` passed.", state.until);
            launcher.close().await;
            return Ok(0);
        }
        save_goal(&mut launcher, sessions.as_ref(), &setup.user, &state, goal.state.as_deref())
            .await?;
        println!("  check `{}` exited {code:?}; iterating.\n", state.until);
    }
    state.status = GoalStatus::Exhausted;
    save_goal(&mut launcher, sessions.as_ref(), &setup.user, &state, goal.state.as_deref()).await?;
    println!("\nbudget exhausted; `{}` still failing. Resume later with --resume.", state.until);
    launcher.close().await;
    Ok(1)
}

async fn read_goal(
    sessions: &dyn SessionService,
    user: &str,
    session: &str,
) -> Result<Option<GoalState>> {
    let session = sessions
        .get(adk_session::GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: Some(1),
            after: None,
        })
        .await?;
    Ok(session
        .state()
        .get(GOAL_STATE_KEY)
        .and_then(|value| serde_json::from_value::<GoalState>(value).ok()))
}

/// Mirrors `state` into the session and, with `--state`, into a JSON file.
async fn save_goal(
    launcher: &mut HarnessLauncher,
    sessions: &dyn SessionService,
    user: &str,
    state: &GoalState,
    mirror: Option<&str>,
) -> Result<()> {
    let mut state = state.clone();
    state.updated_at = chrono::Utc::now();
    let Some(session) = launcher.session_id().map(str::to_string) else {
        return Ok(());
    };
    let mut event = adk_core::Event::new(format!("harness-goal-{}", uuid::Uuid::new_v4()));
    event.author = APP_NAME.to_string();
    event.actions.state_delta.insert(GOAL_STATE_KEY.to_string(), serde_json::to_value(&state)?);
    // The session service checks the session belongs to the user before appending.
    sessions
        .get(adk_session::GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: user.to_string(),
            session_id: session.clone(),
            num_recent_events: Some(1),
            after: None,
        })
        .await?;
    sessions.append_event(&session, event).await?;
    if let Some(path) = mirror {
        let path = Path::new(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&json!({
                "goal": state.goal,
                "until": state.until,
                "iteration": state.iteration,
                "status": serde_json::to_value(state.status)?,
                "lastCheck": state.last_check,
            }))?,
        )
        .with_context(|| format!("failed to write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

/// Runs the goal's success check in the workspace with the agent's shell environment.
///
/// The command comes from the person, not the model, so it is not a governed tool call.
fn run_check(dir: &Path, command: &str) -> (Option<i32>, String) {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command).current_dir(dir).stdin(std::process::Stdio::null());
    let workspace = Workspace::new(dir);
    if !workspace.inherits_env() {
        cmd.env_clear();
        for (key, value) in workspace.bash_env() {
            cmd.env(key, value);
        }
    }
    match cmd.output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            (output.status.code(), text)
        }
        Err(error) => (None, error.to_string()),
    }
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

/// `adk-rust ultracode` on the harness (spec 3.1 §8.1).
pub async fn ultracode(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    task: String,
    max_rounds: i64,
) -> Result<i32> {
    let setup = Arc::new(Setup::new(model, instruction, args, HarnessMode::Ask).await?);
    setup.print_notes();
    let console = ConsoleIo::terminal()?;
    let prompter = setup.unattended_prompter(&console);
    let agent =
        setup.agent(APP_NAME, setup.resolved.mode, setup.resolved.sandbox, prompter, None)?;
    let launcher = setup.launcher(agent).await?.with_console(console);
    let agents = Arc::new(HarnessUltra {
        setup: setup.clone(),
        implementer: tokio::sync::Mutex::new(launcher),
        held: std::sync::Mutex::new(None),
    });
    ultra::run_with(
        agents.clone(),
        &setup.model_id,
        &setup.resolved.workspace.display().to_string(),
        task,
        max_rounds,
    )
    .await?;
    let held = agents.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    Ok(match held {
        Some(outcome) => held_exit(&outcome, persistent(&setup)),
        None => 0,
    })
}

/// Implements in the user's mode in one session; reviews in read-only mode.
struct HarnessUltra {
    setup: Arc<Setup>,
    implementer: tokio::sync::Mutex<HarnessLauncher>,
    held: std::sync::Mutex<Option<TurnOutcome>>,
}

#[async_trait]
impl UltraAgents for HarnessUltra {
    async fn implement(&self, prompt: &str) {
        let mut launcher = self.implementer.lock().await;
        match launcher.run_once(prompt).await {
            Ok(outcome) if !outcome.held.is_empty() => {
                println!("  {} call(s) held for a decision", outcome.held.len());
                *self.held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(outcome);
            }
            Ok(_) => {}
            Err(error) => println!("  implement failed: {error}"),
        }
    }

    async fn review(&self, focus: &str, desc: &str, prompt: &str) -> String {
        let reviewer = async {
            let instruction =
                ultra::review_instruction(focus, desc, self.setup.instruction.as_deref());
            let workspace = Workspace::new(&self.setup.resolved.workspace);
            let agent = HarnessAgent::builder(format!("review-{focus}"))
                .model(self.setup.model.clone())
                .workspace(workspace)
                .mode(HarnessMode::ReadOnly)
                .without_shell()
                .instruction(instruction)
                .build()?;
            let mut launcher = HarnessLauncher::new(agent)
                .app_name(APP_NAME)
                .with_user_id(self.setup.user.clone())
                .with_session_service(Arc::new(InMemorySessionService::new()))
                .with_console(ConsoleIo::scripted([], false))
                .quiet();
            launcher.run_once(prompt).await.map(|outcome| outcome.text)
        };
        reviewer.await.unwrap_or_else(|error: adk_core::AdkError| format!("review failed: {error}"))
    }
}

/// `adk-rust serve` on the harness (spec 3.1 §8.2).
pub async fn serve(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    port: u16,
    host: IpAddr,
    auth_token: Option<String>,
) -> Result<()> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Plan).await?;
    setup.print_notes();
    let loopback = host.is_loopback();
    let token = auth_token.filter(|token| !token.trim().is_empty());
    if !loopback && token.is_none() {
        bail!(
            "serving the harness on {host} needs authentication: pass --auth-token or set \
             ADK_SERVE_TOKEN, or listen on 127.0.0.1"
        );
    }
    if !loopback && setup.resolved.mode == HarnessMode::Auto && !setup.resolved.sandbox {
        bail!(
            "--mode auto beyond loopback needs --sandbox: edits would run unattended on this host"
        );
    }

    #[cfg(feature = "persist")]
    let (approval_store, run_store): (
        Arc<dyn adk_core::ApprovalStore>,
        Arc<dyn adk_core::DurableRunStore>,
    ) = match &setup.db {
        Some(db) => (db.approvals.clone(), db.runs.clone()),
        None => (
            Arc::new(adk_core::InMemoryApprovalStore::new()),
            Arc::new(adk_core::InMemoryRunStore::new()),
        ),
    };
    #[cfg(not(feature = "persist"))]
    let (approval_store, run_store): (
        Arc<dyn adk_core::ApprovalStore>,
        Arc<dyn adk_core::DurableRunStore>,
    ) = (
        Arc::new(adk_core::InMemoryApprovalStore::new()),
        Arc::new(adk_core::InMemoryRunStore::new()),
    );
    // The person running the server is the operator: their own run's calls are theirs to
    // approve, so self-approval is allowed.
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(approval_store)
            .with_router(routing)
            .with_run_store(run_store.clone()),
    );
    let agent = setup.agent(
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        None,
        Some(service.clone()),
    )?;

    // A turn can run as a durable background run of the `adk-rust` target (spec 3.1 §7.12).
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    // wave B: team-graph — add `.with_agent(target, runner)` from `Harness::goal_target()`
    // (spec 3.1 §7.11) so durable goals run here too.
    let background = adk_server::background::BackgroundState::new()
        .with_run_store(run_store)
        .with_app_name(APP_NAME)
        .with_agent(APP_NAME, Arc::new(runner));

    let mut launcher = HarnessLauncher::new(agent)
        .app_name(APP_NAME)
        .with_session_service(setup.sessions())
        .with_user_id(setup.user.clone())
        .with_approval_service(service)
        .with_background_runs(background)
        .with_bind_address(host);
    if let Some(token) = token {
        launcher = launcher.with_request_context_extractor(Arc::new(BearerTokenExtractor::new(
            token,
            setup.user.clone(),
        )));
    }
    launcher.run_serve_directly(port).await.map_err(Into::into)
}
