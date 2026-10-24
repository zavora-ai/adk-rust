//! The terminal surface in the `adk-rust` binary (spec 3.3 §5.2, feature `surface`): full
//! screen by default on a capable terminal, `--ui inline` in the scrollback, `--output json`
//! for CI. It attaches to the running kernel (its socket, or `--server` with a device token)
//! and otherwise runs the harness in this process; either way it is a client of the
//! `adk-surface` engine, and every decision goes through the approvals inbox.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use adk_cli::harness::{HarnessArgs, OPERATOR_SCOPE};
use adk_core::{ApprovalPolicy, ApprovalService, ApproverSelector, StoreApprovalService};
use adk_harness::{Harness, HarnessMode};
use adk_kernel::control::ControlClient;
use adk_surface::tui::{OutputFormat, TerminalSurface, UiMode, terminal_safe};
use adk_surface::{Caller, ControlSource, LocalSource, Origin, SurfaceEngine, SurfaceKind};
use anyhow::{Context, Result};

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
    let ui = match (surface.output, surface.ui) {
        (OutputFormat::Json, _) => None,
        (OutputFormat::Text, Some(ui)) => Some(ui),
        (OutputFormat::Text, None) if TerminalSurface::capable_terminal() => {
            Some(UiMode::FullScreen)
        }
        (OutputFormat::Text, None) => Some(UiMode::Inline),
    };
    if ui == Some(UiMode::FullScreen) {
        if !TerminalSurface::capable_terminal() {
            anyhow::bail!(
                "--ui full needs a terminal of at least 60 by 16 on standard input and output; \
                 use --ui inline or --output json"
            );
        }
        // Logs would draw over the full screen: they go to a file beside harness.db.
        log_to_file();
    }
    let (engine, place, model_id, local) = match reach {
        Reach::Kernel { client, token, local } => {
            let place = if local { "kernel".to_string() } else { client.endpoint().to_string() };
            let mut source = ControlSource::new(client, APP_NAME, APP_NAME);
            if let Some(token) = token {
                source = source.with_bearer_token(token);
            }
            let engine =
                SurfaceEngine::builder(Arc::new(source)).agent_name(APP_NAME, APP_NAME).build();
            (engine, place, None, local)
        }
        Reach::InProcess => {
            let model = model.context("the harness in this process needs a model")?;
            let (engine, model_id) = in_process(model, instruction, harness).await?;
            (engine, "in process".to_string(), Some(model_id), true)
        }
    };
    let mut caller =
        Caller::human(user, SurfaceKind::Terminal, Origin::Keyboard).with_groups([OPERATOR_SCOPE]);
    // The kernel's own socket, or the harness in this process: a terminal on the agents'
    // machine, which may approve high-risk calls and resume after a stop (§7.2, S-I10).
    if local {
        caller = caller.with_paired_local();
    }
    let mut terminal = TerminalSurface::new(engine, caller).theme(surface.theme).place(place);
    if let Some(model_id) = model_id {
        terminal = terminal.model(model_id);
    }
    if let Some(session) = session {
        terminal = terminal.session(session);
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
    Ok(terminal.run(ui.unwrap_or_default()).await?)
}

/// Sends logs to `<config dir>/adk-rust/terminal.log` while the full screen is up.
fn log_to_file() {
    use tracing_subscriber::EnvFilter;
    let Some(dir) = dirs::config_dir().map(|dir| dir.join("adk-rust")) else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(file) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("terminal.log"))
    else {
        return;
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .try_init();
}

/// The harness in this process, held calls going to its approvals inbox, and the engine over
/// it (spec 3.3 §6): the same decisions, the same words, as on a kernel.
async fn in_process(
    model: ResolvedModel,
    instruction: Option<String>,
    args: HarnessArgs,
) -> Result<(SurfaceEngine, String)> {
    let setup = Setup::new(model, instruction, args, HarnessMode::Ask).await?;
    let (approval_store, run_store) = super::up::run_and_approval_stores(&setup);
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(approval_store)
            .with_router(routing)
            .with_run_store(run_store.clone()),
    );
    let agent = Arc::new(setup.agent_with(
        setup.resolved.effort,
        APP_NAME,
        setup.resolved.mode,
        setup.resolved.sandbox,
        None,
        Some(service),
        AgentExtras { local_console: true, ..AgentExtras::default() },
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
    let source = LocalSource::builder(APP_NAME).approvals(decisions).harness(harness).build()?;
    let engine = SurfaceEngine::builder(Arc::new(source)).agent_name(APP_NAME, APP_NAME).build();
    Ok((engine, setup.model_id.clone()))
}
