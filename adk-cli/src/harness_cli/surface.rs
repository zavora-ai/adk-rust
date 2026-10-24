//! The terminal surface in the `adk-rust` binary (spec 3.3 §5.2, feature `tui`): full
//! screen by default on a capable terminal, `--ui inline` in the scrollback, `--output json`
//! for CI. It attaches to the running kernel (its socket, or `--server` with a device token)
//! and otherwise runs the harness in this process; either way it is a client of the
//! `adk-surface` engine, and every decision goes through the approvals inbox.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_cli::harness::{HarnessArgs, KernelSettings, OPERATOR_SCOPE};
use adk_core::{ApprovalPolicy, ApprovalService, ApproverSelector, StoreApprovalService, UserId};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessMode, PromptAnswer, ToolKind, TrustDecision,
};
use adk_kernel::control::ControlClient;
use adk_session::{CreateRequest, GetRequest};
use adk_surface::tui::{OutputFormat, TerminalSurface, UiMode, terminal_safe};
use adk_surface::{Caller, ControlSource, LocalSource, Origin, SurfaceEngine, SurfaceKind};
use anyhow::{Context, Result};

use super::{APP_NAME, AgentExtras, Setup};
use crate::ResolvedModel;
use crate::cli::SurfaceArgs;

/// Where the terminal's agents run.
pub enum Reach {
    /// A kernel: on its own machine over its socket (`paired_local`, with its `home`), or
    /// over HTTP with a bearer token, an operator's or a paired device's.
    Kernel { client: ControlClient, token: Option<String>, local: bool, home: Option<PathBuf> },
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
        return Ok(Reach::Kernel { client, token, local: false, home: None });
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
        return Ok(Reach::Kernel { client, token: None, local: true, home: Some(home) });
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
    let ui = match (surface.output, surface.ui) {
        (OutputFormat::Json, _) => None,
        (OutputFormat::Text, Some(ui)) => Some(ui),
        (OutputFormat::Text, None) if TerminalSurface::capable_terminal() => {
            Some(UiMode::FullScreen)
        }
        (OutputFormat::Text, None) => Some(UiMode::Inline),
    };
    let mut log = None;
    if ui == Some(UiMode::FullScreen) {
        if !TerminalSurface::capable_terminal() {
            anyhow::bail!(
                "--ui full needs a terminal of at least 60 by 16 on standard input and output; \
                 use --ui inline or --output json"
            );
        }
        // Logs would draw over the full screen: they go to a file beside harness.db.
        log = log_to_file();
    }
    let text_ui = surface.output == OutputFormat::Text;
    let (engine, place, model_id, local, session, notices) = match reach {
        Reach::Kernel { client, token, local, home } => {
            let place = if local { "kernel".to_string() } else { client.endpoint().to_string() };
            let mut source = ControlSource::new(client, APP_NAME, APP_NAME);
            if let Some(token) = token {
                source = source.with_bearer_token(token);
            }
            // The control plane does not report the daily spending limit; on the kernel's
            // own machine it is the one `adk-rust up` read from the same files (spec 3.3 §13
            // Q7), so `--output json` can end with 4.
            let workspace = match &harness.dir {
                Some(dir) => dir.clone(),
                None => std::env::current_dir()?,
            };
            if let Some(home) = &home
                && let Some(limit) =
                    kernel_daily_limit(super::user_config().as_deref(), home, &workspace)
            {
                source = source.with_daily_limit(limit);
            }
            let engine =
                SurfaceEngine::builder(Arc::new(source)).agent_name(APP_NAME, APP_NAME).build();
            (engine, place, None, local, session, Vec::new())
        }
        Reach::InProcess => {
            let model = model.context("the harness in this process needs a model")?;
            let here = in_process(model, instruction, harness, text_ui).await?;
            (
                here.engine,
                "in process".to_string(),
                Some(here.model_id),
                true,
                Some(here.session),
                here.notices,
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
    if surface.output == OutputFormat::Json {
        let message = match surface.message {
            Some(message) => Some(message),
            None if !std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                let mut text = String::new();
                std::io::stdin().read_to_string(&mut text)?;
                Some(text).filter(|text| !text.trim().is_empty())
            }
            None => None,
        };
        return Ok(terminal.run_json(message, std::io::stdout()).await?);
    }
    #[cfg(not(unix))]
    drop(log);
    #[cfg(unix)]
    let stderr = match (&log, ui) {
        (Some(log), Some(UiMode::FullScreen)) => StderrToLog::redirect(log),
        _ => None,
    };
    let code = terminal.run(ui.unwrap_or_default()).await;
    #[cfg(unix)]
    drop(stderr);
    Ok(code?)
}

/// The person at this terminal. The kernel's own socket, or the harness in this process: a
/// terminal on the agents' machine, which may approve high-risk calls and resume after a stop
/// (§7.2, S-I10); over HTTP it never is.
fn terminal_caller(user: String, local: bool) -> Caller {
    let caller =
        Caller::human(user, SurfaceKind::Terminal, Origin::Keyboard).with_groups([OPERATOR_SCOPE]);
    if local { caller.with_paired_local() } else { caller }
}

/// The daily spending limit of the kernel at `home`, as `adk-rust up` set it: the user
/// config's `[kernel.quotas] daily_spend_cap_usd`, else the personal preset's answer kept in
/// the home (spec 3.3 §13 Q7).
fn kernel_daily_limit(user_config: Option<&Path>, home: &Path, workspace: &Path) -> Option<u64> {
    let settings = KernelSettings::resolve(user_config, workspace).ok()?;
    settings.quotas.daily_spend_cap.or_else(|| {
        adk_harness::personal::PersonalSettings::load(home)
            .ok()
            .flatten()?
            .daily_spending_limit_micro_usd()
    })
}

/// Standard error goes to `terminal.log` while the full screen is up, so what a tool server
/// or a library writes there never draws over it, nor reaches the terminal unescaped (S-I9).
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

/// Sends logs to `<config dir>/adk-rust/terminal.log` while the full screen is up; returns the
/// file, for standard error too.
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

/// What the harness in this process gives the terminal.
struct InProcess {
    engine: SurfaceEngine,
    model_id: String,
    session: String,
    /// What the terminal says when it starts: a workspace not trusted, memory turned on.
    notices: Vec<String>,
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
    // Long-term memory stays on, as in the REPL and in `up` (spec 3.1 §7.15.2, 3.2 §8.1).
    let agent = Arc::new(setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        prompter,
        Some(service),
        AgentExtras { local_console: true, memory_with_inbox: true, ..AgentExtras::default() },
    )?);
    let runner = adk_runner::Runner::builder()
        .app_name(APP_NAME)
        .agent(agent.agent())
        .session_service(setup.sessions())
        .approval_store(agent.runtime().approval_store().clone())
        .build()?;
    let decisions = agent
        .runtime()
        .approval_service()
        .cloned()
        .context("the harness has an approval service")?;
    let harness = Arc::new(Harness::with_runner(agent, runner)?.with_run_store(run_store));
    let session = open_session(&setup).await?;
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
    let source = LocalSource::builder(APP_NAME).approvals(decisions).harness(harness).build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).agent_name(APP_NAME, APP_NAME).build();
    Ok(InProcess { engine, model_id: setup.model_id.clone(), session, notices })
}

/// Where the terminal's memory notice points: the commands, as the terminal has no `/memory`.
const MEMORY_COMMANDS: &str =
    "`adk-rust memory list` lists them, `adk-rust memory forget ID` removes one";

/// The state key a session's workspace is recorded under (spec 3.1 §8.1).
const WORKSPACE: &str = "harness:workspace";

/// The session the terminal opens in process: `--session`, else with `--continue` the latest
/// in this workspace, else a new one. A session is bound to its workspace as in the REPL: a new
/// one records it, and one recorded for another workspace is refused (spec 3.1 §8.1).
async fn open_session(setup: &Setup) -> Result<String> {
    let sessions = setup.sessions();
    let workspace = &setup.resolved.workspace;
    let requested = match &setup.args.session {
        Some(session) => Some(session.clone()),
        None if setup.args.continue_session => {
            super::latest_in_workspace(sessions.as_ref(), &setup.user, workspace, |_| true).await?
        }
        None => None,
    };
    let recorded = workspace.display().to_string();
    if let Some(session_id) = &requested {
        let existing = sessions
            .get(GetRequest {
                app_name: APP_NAME.to_string(),
                user_id: setup.user.clone(),
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
                    && other != recorded
                {
                    anyhow::bail!(
                        "session {session_id} works in {other}, not {recorded}; continue it with \
                         --dir {other}, or start a new session here"
                    );
                }
                return Ok(session.id().to_string());
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    let created = sessions
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: setup.user.clone(),
            session_id: requested,
            state: HashMap::from([(WORKSPACE.to_string(), serde_json::json!(recorded))]),
        })
        .await?;
    Ok(created.id().to_string())
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

    /// `--output json` can end with 4 on a kernel on this machine: its daily spending limit is
    /// read from the files `adk-rust up` read (spec 3.3 §13 Q7).
    #[test]
    fn a_local_kernels_daily_limit_comes_from_the_files_up_reads() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[kernel.quotas]\ndaily_spend_cap_usd = 2.5\n").unwrap();
        assert_eq!(
            kernel_daily_limit(Some(config.as_path()), dir.path(), dir.path()),
            Some(2_500_000)
        );
        assert_eq!(kernel_daily_limit(None, dir.path(), dir.path()), None);
    }
}
