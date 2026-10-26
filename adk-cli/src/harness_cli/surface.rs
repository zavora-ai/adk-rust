//! The terminal surface in the `adk-rust` binary (spec 3.3 §5.2, feature `tui`): full
//! screen by default on a capable terminal, `--ui inline` in the scrollback, `--output json`
//! for CI. It attaches to the running kernel (its socket, or `--server` with a device token)
//! and otherwise runs the harness in this process; either way it is a client of the
//! `adk-surface` engine, and every decision goes through the approvals inbox.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use adk_cli::harness::{HarnessArgs, HarnessCommands, OPERATOR_SCOPE};
use adk_core::{ApprovalPolicy, ApprovalService, ApproverSelector, StoreApprovalService, UserId};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessMode, PromptAnswer, ToolKind, TrustDecision,
};
use adk_kernel::control::api::AreaView;
use adk_kernel::control::{CommandInfo, ControlClient};
use adk_session::{CreateRequest, GetRequest, SessionService};
use adk_surface::tui::{
    CommandContext, OutputFormat, TerminalCommand, TerminalSurface, UiMode, terminal_safe,
};
use adk_surface::{Caller, ControlSource, LocalSource, Origin, SurfaceEngine, SurfaceKind};
use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use super::{APP_NAME, AgentExtras, Setup};
use crate::ResolvedModel;
use crate::cli::SurfaceArgs;

/// Where the terminal's agents run.
pub enum Reach {
    /// A kernel: on its own machine over its socket (`paired_local`), or over HTTP with a
    /// bearer token, an operator's or a paired device's.
    Kernel { client: ControlClient, token: Option<String>, local: bool },
    /// No kernel runs: the harness, in this process.
    InProcess,
}

/// The kernel the terminal attaches to: `--server` (with `--token`, `$ADK_SERVE_TOKEN`, or the
/// device token `--pair` kept), else the one at `--home` or the default home, unless
/// `--ephemeral` or `--continue` keep the harness in this process.
pub async fn reach(surface: &SurfaceArgs, harness: &HarnessArgs) -> Result<Reach> {
    let target = &surface.kernel;
    if let Some(server) = &target.server {
        let server = server.trim_end_matches('/');
        let token = match &surface.pair {
            Some(code) => Some(pair(server, code).await?),
            None => target
                .token
                .clone()
                .or_else(|| std::env::var("ADK_SERVE_TOKEN").ok())
                .filter(|token| !token.trim().is_empty())
                .or_else(|| kept_token(server)),
        };
        let token = token.filter(|token| !token.trim().is_empty());
        let mut client = ControlClient::http(server);
        if let Some(token) = &token {
            client = client.with_bearer_token(token.clone());
        }
        return Ok(Reach::Kernel { client, token, local: false });
    }
    if harness.ephemeral || harness.continue_session {
        return Ok(Reach::InProcess);
    }
    let home = match &target.home {
        Some(home) => Some(home.clone()),
        None => dirs::config_dir().map(|dir| dir.join("adk-rust")),
    };
    if let Some(home) = home
        && let Some(client) = ControlClient::connect_local(&home).await?
    {
        return Ok(Reach::Kernel { client, token: None, local: true });
    }
    Ok(Reach::InProcess)
}

/// Where `--pair` keeps the device token for the kernel at `server`: one owner-only file per
/// kernel, under the config directory.
fn token_path(server: &str) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(server.as_bytes()));
    let dir = dirs::config_dir()?.join("adk-rust").join("devices");
    Some(dir.join(format!("{}.token", &digest[..16])))
}

/// The device token `--pair` kept for `server`, if any.
fn kept_token(server: &str) -> Option<String> {
    let token = std::fs::read_to_string(token_path(server)?).ok()?;
    Some(token.trim().to_string()).filter(|token| !token.is_empty())
}

/// Pairs this terminal with the kernel at `server` by the code `adk-rust pair` showed there,
/// as a phone pairs (spec 3.3 §5.2, §10 K2), and keeps the device token, readable by its
/// owner only, for the next start.
async fn pair(server: &str, code: &str) -> Result<String> {
    let name = format!("{}'s terminal", crate::setup::os_user());
    let claimed =
        ControlClient::http(server).claim_pairing(code.trim(), Some(&name)).await.context(
            "pairing failed: use a code from `adk-rust pair` on the kernel's machine within ten \
             minutes; each code pairs one device",
        )?;
    let path = token_path(server)
        .context("could not determine the config directory to keep the device token in")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A fresh file, so it is created owner-only even where an older one was not.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(not(windows))]
    let created = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&path)
    };
    // A DACL granting the owner alone access stands in for mode 0600 (spec 3.2 §10 W4).
    #[cfg(windows)]
    let created = crate::owner_only::open_append(&path);
    let mut file = created.with_context(|| format!("failed to create {}", path.display()))?;
    std::io::Write::write_all(&mut file, claimed.token.as_bytes())?;
    let scopes: Vec<&str> = claimed.device.scopes.iter().map(|scope| scope.as_str()).collect();
    eprintln!(
        "Paired as {} ({}) until {}; the token is kept in {}.",
        terminal_safe(&claimed.device.name),
        scopes.join(", "),
        claimed.device.expires_at.with_timezone(&chrono::Local).format("%Y-%m-%d"),
        path.display()
    );
    Ok(claimed.token)
}

/// Runs the terminal on `reach` and returns the exit code (spec 3.3 §5.2). `model` is needed
/// only in process.
pub async fn run(
    reach: Reach,
    model: Option<ResolvedModel>,
    instruction: Option<String>,
    harness: HarnessArgs,
    surface: SurfaceArgs,
) -> Result<i32> {
    let session = harness.session.clone();
    let user = crate::setup::os_user();
    let ui = match (surface.output, surface.ui.and_then(crate::cli::UiChoice::surface)) {
        (OutputFormat::Json, _) => None,
        (OutputFormat::Text, Some(ui)) => Some(ui),
        (OutputFormat::Text, None) if TerminalSurface::capable_terminal() => {
            Some(UiMode::FullScreen)
        }
        (OutputFormat::Text, None) => Some(UiMode::Inline),
    };
    if ui == Some(UiMode::FullScreen) && !TerminalSurface::capable_terminal() {
        anyhow::bail!(
            "--ui full needs a terminal of at least 60 by 16 on standard input and output; use \
             --ui inline or --output json"
        );
    }
    // The inline UI on a terminal holds raw mode and the prompt line, as the full screen
    // holds the screen; piped, standard error is the caller's to keep.
    let interactive_inline = ui == Some(UiMode::Inline)
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
        && std::env::var("TERM").map_or(true, |term| term != "dumb");
    let drawn = ui == Some(UiMode::FullScreen) || interactive_inline;
    // Logs would draw over the terminal: they go to a file beside harness.db.
    let log = if drawn { log_to_file() } else { None };
    let text_ui = surface.output == OutputFormat::Text;
    let mut ending = None;
    let (engine, place, model_id, local, session, notices, binding, commands) = match reach {
        Reach::Kernel { client, token, local } => {
            let place = if local { "kernel".to_string() } else { client.endpoint().to_string() };
            // How each answer was given comes from the kernel's record, so a decision answered
            // in Telegram reads "You said yes in Telegram" here too (spec 3.3 §12 phase 1).
            let answers = Arc::new(adk_surface::RecordAnswerLog::new(client.clone()));
            let source_client = client.clone();
            let mut source = ControlSource::new(client, APP_NAME, APP_NAME);
            if let Some(token) = token {
                source = source.with_bearer_token(token);
            }
            // The daily spending limit in force, as the kernel reports it, so `--output json`
            // can end with 4 (spec 3.3 §10 K3, §13 Q7).
            if let Ok(areas) = source_client.settings().await
                && let Some(limit) = area_limit(&areas)
            {
                source = source.with_daily_limit(limit);
            }
            let (commands, notices, operator) = kernel_commands(&source_client).await;
            let trail = source.audit_trail();
            let mut engine = SurfaceEngine::builder(Arc::new(source))
                .agent_name(APP_NAME, APP_NAME)
                .answers(answers);
            // Every answer and stop, with the surface and how it was given, on the kernel's
            // signed log (spec 3.3 §11); a paired device's answers the kernel records itself.
            if operator {
                engine = engine.audit(trail);
            }
            (engine.build(), place, None, local, session, notices, None, commands)
        }
        Reach::InProcess => {
            let model = model.context("the harness in this process needs a model")?;
            let here = in_process(model, instruction, harness, text_ui).await?;
            ending = Some(here.ending);
            (
                here.engine,
                "in process".to_string(),
                Some(here.model_id),
                true,
                Some(here.session),
                here.notices,
                Some(here.binding),
                here.commands,
            )
        }
    };
    let caller = terminal_caller(user, local);
    let mut terminal = TerminalSurface::new(engine, caller).theme(surface.theme).place(place);
    if let Some(model_id) = model_id {
        terminal = terminal.model(model_id);
    }
    if let Some(session) = session {
        terminal = terminal.session(session);
    }
    for notice in notices {
        terminal = terminal.notice(notice);
    }
    // `/session` binds a session to the workspace as the start does: a new one records it,
    // and one recorded for another workspace is refused (spec 3.1 §8.1).
    if let Some(binding) = binding {
        terminal = terminal.open_sessions_with(move |requested| {
            let binding = binding.clone();
            Box::pin(
                async move { binding.open(requested).await.map_err(|error| error.to_string()) },
            )
        });
    }
    for command in commands {
        terminal = terminal.command(command);
    }
    // What long-term memory caught up on reaches the terminal as it arrives (spec 3.1 §7.33
    // fix 9).
    if let Some(late) = ending.as_mut().and_then(|ending| ending.late.take()) {
        terminal = terminal.notices_from(late);
    }
    let code = if surface.output == OutputFormat::Json {
        let message = match surface.message {
            Some(message) => Some(message),
            None if !std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                Some(text).filter(|text| !text.trim().is_empty())
            }
            None => None,
        };
        terminal.run_json(message, std::io::stdout()).await?
    } else {
        #[cfg(not(unix))]
        drop(log);
        #[cfg(unix)]
        let stderr = match &log {
            Some(log) if drawn => StderrToLog::redirect(log),
            _ => None,
        };
        let code = terminal.run(ui.unwrap_or_default()).await;
        #[cfg(unix)]
        drop(stderr);
        code?
    };
    // A signal ends the process now, as it asked: its sessions are left to the next start's
    // catch-up (spec 3.1 §7.33 fix 9).
    if let Some(ending) = ending
        && !matches!(code, 129 | 130 | 143)
    {
        ending.finish(text_ui).await;
    }
    Ok(code)
}

/// The REPL's slash commands the kernel serves, each run there in the thread the terminal
/// shows (spec 3.3 §5.2), what the terminal says when it has none, and whether the kernel
/// admits the terminal as the operator: a paired device's token reaches none of them.
async fn kernel_commands(
    client: &ControlClient,
) -> (Vec<Arc<dyn TerminalCommand>>, Vec<String>, bool) {
    match client.commands().await {
        Ok(listed) => {
            let commands = listed
                .into_iter()
                .map(|info| {
                    Arc::new(KernelCommand { client: client.clone(), info })
                        as Arc<dyn TerminalCommand>
                })
                .collect();
            (commands, Vec::new(), true)
        }
        Err(error)
            if matches!(
                error.category(),
                adk_core::ErrorCategory::Forbidden | adk_core::ErrorCategory::Unauthorized
            ) =>
        {
            (Vec::new(), vec![PAIRED_TERMINAL.to_string()], false)
        }
        Err(error) => {
            tracing::debug!(error = %error, "the kernel offers no commands");
            (Vec::new(), Vec::new(), true)
        }
    }
}

/// What a terminal paired as a device says as it starts: the REPL's commands are the kernel
/// operator's (spec 3.3 §5.2).
const PAIRED_TERMINAL: &str = "The REPL's commands (/todos, /rules, /memory, ...) run only on \
                               the kernel's own machine or with its operator token; this paired \
                               terminal has the terminal's own.";

/// One of the REPL's commands, run on the kernel the terminal is attached to.
struct KernelCommand {
    client: ControlClient,
    info: CommandInfo,
}

#[async_trait::async_trait]
impl TerminalCommand for KernelCommand {
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        &self.info.description
    }

    async fn run(&self, args: &str, context: &CommandContext) -> adk_core::Result<String> {
        Ok(self.client.run_command(&self.info.name, args, &context.session_id).await?)
    }
}

/// The person at this terminal. The kernel's own socket, or the harness in this process: a
/// terminal on the agents' machine, which may approve high-risk calls and resume after a stop
/// (§7.2, S-I10); over HTTP it never is.
fn terminal_caller(user: String, local: bool) -> Caller {
    let caller =
        Caller::human(user, SurfaceKind::Terminal, Origin::Keyboard).with_groups([OPERATOR_SCOPE]);
    if local { caller.with_paired_local() } else { caller }
}

/// The daily spending limit in force for the terminal's turns: the default area's, or the
/// only area's; none when the kernel has several and none is the default.
fn area_limit(areas: &[AreaView]) -> Option<u64> {
    let only = match areas {
        [only] => Some(only),
        _ => None,
    };
    areas
        .iter()
        .find(|area| area.partition.as_str() == adk_kernel::PartitionId::DEFAULT)
        .or(only)?
        .daily_spending_limit
}

/// Standard error goes to `terminal.log` while the full screen or the inline UI is up, so
/// what a tool server or a library writes there never draws over it, nor reaches the terminal
/// unescaped (S-I9).
#[cfg(unix)]
struct StderrToLog {
    saved: std::os::fd::OwnedFd,
}

#[cfg(unix)]
impl StderrToLog {
    fn redirect(log: &std::fs::File) -> Option<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        // SAFETY: `dup` and `dup2` act on descriptors this process holds open; the copy of
        // standard error is owned by `saved` from here on.
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return None;
            }
            if libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO) < 0 {
                libc::close(saved);
                return None;
            }
            Some(Self { saved: std::os::fd::OwnedFd::from_raw_fd(saved) })
        }
    }
}

#[cfg(unix)]
impl Drop for StderrToLog {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        // SAFETY: both descriptors are open; standard error gets its terminal back.
        unsafe {
            libc::dup2(self.saved.as_raw_fd(), libc::STDERR_FILENO);
        }
    }
}

/// Sends logs to `<config dir>/adk-rust/terminal.log` while the full screen or the inline UI
/// is up; returns the file, for standard error too.
fn log_to_file() -> Option<std::fs::File> {
    use tracing_subscriber::EnvFilter;
    let dir = dirs::config_dir().map(|dir| dir.join("adk-rust"))?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("terminal.log"))
        .ok()?;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    if let Ok(writer) = file.try_clone() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(writer))
            .with_ansi(false)
            .try_init();
    }
    Some(file)
}

/// What the terminal in process leaves to do when it ends.
struct Ending {
    harness: Arc<Harness>,
    /// The notices of long-term memory catching up on the sessions that ended without saving
    /// (spec 3.1 §7.33 fix 9), until the terminal takes them.
    late: Option<UnboundedReceiver<String>>,
    /// Waits for the catch-up and hands its notice to the terminal; ends with the notice
    /// when the terminal had ended.
    catch_up: Option<JoinHandle<Option<String>>>,
    memory: bool,
}

impl Ending {
    /// Waits for the catch-up, then closes the harness, distilling this run's sessions into
    /// long-term memory as the REPL does when it ends (spec 3.1 §7.15.2). `text` says whether
    /// to print, which JSON lines on standard output rule out.
    async fn finish(self, text: bool) {
        let left = match self.catch_up {
            Some(catch_up) => catch_up.await.ok().flatten(),
            None => None,
        };
        if text && let Some(notice) = left {
            println!("{notice}");
        }
        if text && self.memory {
            println!("Saving what this session taught to long-term memory…");
        }
        for (session, error) in self.harness.close_with_report().await.distillation_failures {
            if text {
                println!(
                    "note: long-term memory was not updated for session {}: {error}",
                    adk_harness::escape_control(&session)
                );
            } else {
                tracing::warn!(session.id = %session, error = %error, "session distillation at close failed");
            }
        }
    }
}

/// What the harness in this process gives the terminal.
struct InProcess {
    engine: SurfaceEngine,
    model_id: String,
    session: String,
    /// What the terminal says when it starts: a workspace not trusted, memory turned on.
    notices: Vec<String>,
    /// How a session is bound to the workspace, for `/session` too.
    binding: Binding,
    /// The REPL's slash commands, on this harness and session (spec 3.3 §5.2).
    commands: Vec<Arc<dyn TerminalCommand>>,
    ending: Ending,
}

/// The harness in this process, held calls going to its approvals inbox, and the engine over
/// it (spec 3.3 §6): the same decisions, the same words, as on a kernel. It keeps what the REPL
/// it replaces keeps: long-term memory, `--yes`, `--trust`, `--continue` and the session's
/// workspace.
async fn in_process(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
    text_ui: bool,
) -> Result<InProcess> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    let (approval_store, run_store) = super::up::run_and_approval_stores(&setup);
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(approval_store)
            .with_router(routing)
            .with_run_store(run_store.clone()),
    );
    // `--yes` approves held calls as in the REPL, printing nothing over the terminal.
    let prompter = setup.resolved.yes.then(|| {
        Arc::new(ApproveAll { allow_unsandboxed: setup.resolved.allow_unsandboxed })
            as Arc<dyn ApprovalPrompter>
    });
    // With `--yes` the prompter answers held calls, as in the REPL: the harness asks the
    // approval service first when it has one, so it gets none and the inbox stays empty.
    let held_to_inbox = prompter.is_none().then(|| service.clone());
    // Long-term memory stays on, as in the REPL and in `up` (spec 3.1 §7.15.2, 3.2 §8.1).
    let agent = Arc::new(setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        prompter,
        held_to_inbox,
        AgentExtras { local_console: true, memory_with_inbox: true, ..AgentExtras::default() },
    )?);
    // Each run carries the prompter's confirmation handler, as the REPL's do.
    let config = adk_core::RunConfig {
        tool_confirmation_handler: agent.runtime().confirmation_handler().cloned(),
        ..adk_core::RunConfig::default()
    };
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .run_config(config)
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    let decisions = agent.runtime().approval_service().cloned().unwrap_or(service);
    let harness = Arc::new(Harness::with_runner(agent, runner)?.with_run_store(run_store.clone()));
    let binding = Binding {
        sessions: setup.sessions(),
        user: setup.user.clone(),
        workspace: setup.resolved.workspace.display().to_string(),
    };
    let requested = match &setup.args.session {
        Some(session) => Some(session.clone()),
        None if setup.args.continue_session => {
            let workspace = &setup.resolved.workspace;
            super::latest_in_workspace(setup.sessions().as_ref(), &setup.user, workspace, |_| true)
                .await?
        }
        None => None,
    };
    let session = binding.open(requested).await?;
    // The REPL's slash commands, each run in the thread the terminal shows (spec 3.3 §5.2).
    let commands = HarnessCommands::new(harness.clone(), APP_NAME, setup.sessions())
        .run_store(run_store)
        .effort(setup.args.effort)
        .hooks_disabled(!setup.resolved.hooks)
        .terminal(UserId::new(setup.user.clone())?);
    let mut notices = Vec::new();
    // Instruction files and hooks load only in a trusted workspace (spec 3.1 §7.7, §7.22).
    let runtime = harness.agent().runtime();
    if setup.resolved.trust {
        harness.trust_workspace(&UserId::new(setup.user.clone())?, TrustDecision::Trusted).await?;
    } else if !runtime.workspace_trusted() && !runtime.instructions().project_files().is_empty() {
        notices.push(
            "Instruction files in this workspace are not loaded: it is not trusted. Pass --trust \
             to load them."
                .to_string(),
        );
    }
    if text_ui && let Some(notice) = setup.memory_notice_text(MEMORY_COMMANDS) {
        notices.push(notice.replace('\n', " "));
    }
    // Sessions that ended without saving are distilled in the background (spec 3.1 §7.33
    // fix 9); JSON output answers one message and is no REPL.
    let user = UserId::new(setup.user.clone())?;
    let mut ending =
        Ending { harness: harness.clone(), late: None, catch_up: None, memory: setup.memory_on() };
    if text_ui
        && let Some(task) = adk_cli::harness::distill_unsaved_sessions(
            harness.clone(),
            setup.sessions(),
            APP_NAME,
            &user,
            &setup.resolved.workspace,
            &session,
        )
    {
        let (tx, late) = tokio::sync::mpsc::unbounded_channel();
        ending.late = Some(late);
        ending.catch_up = Some(tokio::spawn(async move {
            match task.await {
                Ok(Some(notice)) => tx.send(notice).err().map(|unsent| unsent.0),
                _ => None,
            }
        }));
    }
    let source = LocalSource::builder(APP_NAME).approvals(decisions).harness(harness).build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).agent_name(APP_NAME, APP_NAME).build();
    Ok(InProcess {
        engine,
        model_id: setup.model_id.clone(),
        session,
        notices,
        binding,
        commands,
        ending,
    })
}

/// Where the terminal's memory notice points: the REPL's commands, which it has too (spec 3.3
/// §5.2).
const MEMORY_COMMANDS: &str = "/memory lists them, /memory forget ID removes one";

/// The state key a session's workspace is recorded under (spec 3.1 §8.1).
const WORKSPACE: &str = "harness:workspace";

/// How the terminal in process binds a session to its workspace, as the REPL does: a new
/// session records the workspace, and one recorded for another workspace is refused (spec
/// 3.1 §8.1). The start and `/session` both open sessions through it.
#[derive(Clone)]
struct Binding {
    sessions: Arc<dyn SessionService>,
    user: String,
    workspace: String,
}

impl Binding {
    /// Opens `requested`, or a new session for `None`; returns its id.
    async fn open(&self, requested: Option<String>) -> Result<String> {
        if let Some(session_id) = &requested {
            let existing = self
                .sessions
                .get(GetRequest {
                    app_name: APP_NAME.to_string(),
                    user_id: self.user.clone(),
                    session_id: session_id.clone(),
                    num_recent_events: Some(1),
                    after: None,
                })
                .await;
            match existing {
                Ok(session) => {
                    if let Some(other) = session
                        .state()
                        .get(WORKSPACE)
                        .and_then(|value| value.as_str().map(str::to_string))
                        && other != self.workspace
                    {
                        anyhow::bail!(
                            "session {session_id} works in {other}, not {}; continue it with \
                             --dir {other}, or start a new session here",
                            self.workspace
                        );
                    }
                    return Ok(session.id().to_string());
                }
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let created = self
            .sessions
            .create(CreateRequest {
                app_name: APP_NAME.to_string(),
                user_id: self.user.clone(),
                session_id: requested,
                state: HashMap::from([(WORKSPACE.to_string(), serde_json::json!(self.workspace))]),
            })
            .await?;
        Ok(created.id().to_string())
    }
}

/// `--yes` in the terminal: every held call is allowed once, as in the REPL, except a retry
/// outside the sandbox without `--allow-unsandboxed`; nothing is printed over the terminal.
#[derive(Debug)]
struct ApproveAll {
    allow_unsandboxed: bool,
}

#[async_trait::async_trait]
impl ApprovalPrompter for ApproveAll {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        if matches!(prompt.kind, ToolKind::ShellEscalation) && !self.allow_unsandboxed {
            return Ok(PromptAnswer::Deny {
                message: Some(
                    "--yes does not approve unsandboxed retries without --allow-unsandboxed".into(),
                ),
            });
        }
        Ok(PromptAnswer::ApproveOnce)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;

    /// Only the kernel's own socket and the harness in process are a paired local terminal
    /// (spec 3.3 §7.2, S-I10): over HTTP, with an operator's token or a device's, it never is.
    #[tokio::test]
    async fn only_the_socket_and_in_process_are_paired_local() {
        let remote = Cli::try_parse_from([
            "adk-rust",
            "--server",
            "http://127.0.0.1:9",
            "--token",
            "operator",
        ])
        .unwrap();
        let reached = reach(&remote.surface, &remote.harness).await.unwrap();
        assert!(matches!(reached, Reach::Kernel { local: false, .. }));

        let here = Cli::try_parse_from(["adk-rust", "--ephemeral"]).unwrap();
        let reached = reach(&here.surface, &here.harness).await.unwrap();
        assert!(matches!(reached, Reach::InProcess));

        assert!(!terminal_caller("ana".into(), false).paired_local);
        assert!(terminal_caller("ana".into(), true).paired_local);
    }

    /// The paired terminal's notice reads as one sentence, with no run of spaces from a lost
    /// line continuation (wave 3 review).
    #[test]
    fn the_paired_terminals_notice_is_one_sentence() {
        assert!(!PAIRED_TERMINAL.contains("  "), "{PAIRED_TERMINAL}");
        assert!(PAIRED_TERMINAL.ends_with("this paired terminal has the terminal's own."));
    }

    /// `--output json` ends with 4 against the daily spending limit in force, as the kernel
    /// reports it (spec 3.3 §10 K3): the default area's, or the only area's.
    #[test]
    fn the_daily_limit_is_the_one_the_kernel_reports() {
        let area = |name: &str, limit: Option<u64>| -> AreaView {
            serde_json::from_value(serde_json::json!({
                "partition": name,
                "dailySpendingLimit": limit,
                "spentToday": 0,
            }))
            .unwrap()
        };
        assert_eq!(area_limit(&[area("default", Some(10_000_000))]), Some(10_000_000));
        assert_eq!(
            area_limit(&[area("billing", Some(1)), area("default", Some(2_500_000))]),
            Some(2_500_000)
        );
        assert_eq!(area_limit(&[area("billing", Some(1))]), Some(1));
        assert_eq!(area_limit(&[area("billing", Some(1)), area("home", Some(2))]), None);
        assert_eq!(area_limit(&[area("default", None)]), None);
        assert_eq!(area_limit(&[]), None);
    }

    /// `/session` binds a session to the workspace as the start does: a session recorded for
    /// another workspace is refused, and a new one records this workspace (spec 3.1 §8.1).
    #[tokio::test]
    async fn slash_session_binds_sessions_to_the_workspace() {
        let sessions: Arc<dyn SessionService> =
            Arc::new(adk_session::InMemorySessionService::new());
        let ws =
            Binding { sessions: sessions.clone(), user: "ana".into(), workspace: "/ws".into() };
        assert_eq!(ws.open(Some("pinned".into())).await.unwrap(), "pinned");
        let other = Binding { workspace: "/other".into(), ..ws.clone() };
        let refused = other.open(Some("pinned".into())).await.unwrap_err().to_string();
        assert!(refused.contains("session pinned works in /ws, not /other"), "{refused}");
        let fresh = other.open(None).await.unwrap();
        let created = sessions
            .get(GetRequest {
                app_name: APP_NAME.to_string(),
                user_id: "ana".into(),
                session_id: fresh,
                num_recent_events: Some(1),
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(created.state().get(WORKSPACE), Some(serde_json::json!("/other")));
    }
}
