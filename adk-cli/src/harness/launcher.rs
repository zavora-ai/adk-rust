//! `HarnessLauncher`: the harness REPL, one-turn runs, the served harness (spec 3.1 §8.1,
//! §8.2) and its resident form on the agent kernel (spec 3.2 §7.1, §8.1).

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};

use adk_core::{
    AdkError, Agent, ApprovalService, DurableRunStore, Effort, Event, GovernanceControl, Result,
    RunConfig, SessionId, ToolConfirmationRequest, UserId, held_tool_calls,
};
use adk_harness::{Harness, HarnessAgent, HarnessMode, TrustDecision};
use adk_runner::Runner;
use adk_server::background::BackgroundState;
use adk_server::{RequestContextExtractor, SecurityConfig, shutdown_signal};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use axum::Router;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::commands::harness_commands;
use super::serve::LoopbackOperatorExtractor;
use crate::console::{ConsoleCommandContext, ConsoleInput, ConsoleIo};
use crate::launcher::{
    CONSOLE_CONFIRMATION_TIMEOUT, Launcher, ThinkingDisplayMode, command_completions, drive_turn,
    find_command, help_text, is_exit_command, terminal_safe,
};

/// Session-state key recording the canonical workspace a harness session works in
/// (spec 3.1 §8.1).
pub(crate) const WORKSPACE_STATE_KEY: &str = "harness:workspace";

/// How long a served request may take: a harness turn runs model calls and commands, often
/// for minutes, and the server's 30-second default would answer `408` mid-turn.
const SERVE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Rebuilds the harness with the OS sandbox on (`true`) or off, for `/sandbox on|off`.
pub type HarnessRebuild = Arc<dyn Fn(bool) -> Result<HarnessAgent> + Send + Sync>;

/// Rebuilds the harness with every mode and role at another effort, for
/// `/effort low|medium|high` (spec 3.1 §7.23); the harness sets each request's effort, so
/// the next model call runs at it on the same client.
pub type HarnessEffortRebuild =
    Arc<dyn Fn(Effort) -> Pin<Box<dyn Future<Output = Result<HarnessAgent>> + Send>> + Send + Sync>;

/// How one [`HarnessLauncher::run_once`] turn ended (spec 3.1 §8.1).
///
/// # Example
///
/// ```rust,ignore
/// let outcome = launcher.run_once("make the failing test pass").await?;
/// if !outcome.held.is_empty() {
///     eprintln!("calls are held in session {}", outcome.session_id);
///     std::process::exit(3);
/// }
/// ```
#[derive(Clone, Debug, Default)]
pub struct TurnOutcome {
    /// The session the turn ran in.
    pub session_id: String,
    /// Calls held for a decision; decide them with `adk-rust approvals` and resume.
    pub held: Vec<ToolConfirmationRequest>,
    /// The run was cancelled with Ctrl-C.
    pub interrupted: bool,
    /// The turn ended after a person denied a call (spec 3.1 §9.2 I19).
    pub ended_by_denial: bool,
    /// What the agent wrote.
    pub text: String,
}

/// Runs a [`HarnessAgent`] at the console or behind the HTTP server (spec 3.1 §8.1, §8.2).
///
/// - [`run`](Self::run) parses `serve [--port] [--host]`; without it the REPL starts.
/// - The REPL shows a banner with the workspace, mode, isolation, members and session,
///   asks once whether to trust the workspace's instruction files, and offers
///   `/mode`, `/sandbox`, `/todos`, `/rules`, `/approvals`, `/plan`, `/cost`, `/team`,
///   `/session`, `/skills` and `/memory`. Approvals are asked by the prompter the harness
///   was built with, such as [`ConsolePrompter`](super::ConsolePrompter); after a person
///   denies a call the turn ends.
/// - [`run_once`](Self::run_once) runs one turn and reports calls held for a decision.
/// - A session records its canonical workspace and refuses to continue from another one.
/// - Serve mode mounts the harness behind an authenticating extractor: loopback peers are
///   the operator by default ([`LoopbackOperatorExtractor`]); any other address needs
///   one set with [`with_request_context_extractor`](Self::with_request_context_extractor),
///   such as [`BearerTokenExtractor`](super::BearerTokenExtractor).
/// - With a kernel ([`with_kernel`](Self::with_kernel)) serve mode is the resident form
///   (spec 3.2 §8.1): the kernel's workers execute the runs, its control plane is mounted
///   under `/api/kernel` and, when its configuration serves one, on its socket, and
///   `POST /api/admin/freeze` parks running processes. [`run`](Self::run) parses
///   `up [--port] [--host]` for it, with the socket at the home.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_cli::console::ConsoleIo;
/// use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
/// use adk_harness::HarnessAgent;
///
/// async fn run(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
///     let harness = HarnessAgent::builder("harness")
///         .model(model)
///         .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
///         .prompter(Arc::new(ConsolePrompter::new(ConsoleIo::terminal()?)))
///         .build()?;
///     // `cargo run` is the REPL; `cargo run -- serve` serves the harness on loopback.
///     HarnessLauncher::new(harness).run().await
/// }
/// ```
pub struct HarnessLauncher {
    agent: Arc<HarnessAgent>,
    app_name: Option<String>,
    session_service: Option<Arc<dyn SessionService>>,
    user_id: String,
    session_id: Option<String>,
    mode: Option<HarnessMode>,
    trust: bool,
    console: Option<ConsoleIo>,
    thinking_mode: ThinkingDisplayMode,
    pub(super) echo: bool,
    rebuild: Option<HarnessRebuild>,
    effort_rebuild: Option<HarnessEffortRebuild>,
    effort: Option<Effort>,
    status_lines: Vec<(String, String)>,
    banner_notes: Vec<String>,
    request_context_extractor: Option<Arc<dyn RequestContextExtractor>>,
    auth_token: Option<String>,
    governance: Option<GovernanceControl>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    background_runs: Option<BackgroundState>,
    security_config: Option<SecurityConfig>,
    bind_address: IpAddr,
    run_store: Option<Arc<dyn DurableRunStore>>,
    goal_queue: Option<String>,
    kernel: Option<adk_kernel::Kernel>,
    hooks_disabled: bool,
    opened: Option<Arc<Shared>>,
    #[cfg(feature = "surface")]
    surface_records: Option<crate::surface::SurfaceRecords>,
}

impl std::fmt::Debug for HarnessLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessLauncher")
            .field("agent", &self.agent)
            .field("app_name", &self.app_name)
            .field("user_id", &self.user_id)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl HarnessLauncher {
    /// A launcher for `agent`, running as user `"user"` in the app named after the agent,
    /// with in-memory sessions, until configured otherwise.
    pub fn new(agent: HarnessAgent) -> Self {
        Self::shared(Arc::new(agent))
    }

    /// [`new`](Self::new) for an agent other parts of the program hold too, such as the
    /// `Harness` a resident kernel is built from with `Harness::kernel` (spec 3.2 §10 HK1):
    /// the served inbox then decides through the same agent's approval service, which wakes
    /// the kernel's approval sleepers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = Arc::new(harness_agent);
    /// let harness = Harness::with_runner(agent.clone(), runner)?;
    /// let kernel = harness.kernel().build()?;
    /// HarnessLauncher::shared(agent).with_kernel(kernel).run_serve_directly(8080).await?;
    /// ```
    pub fn shared(agent: Arc<HarnessAgent>) -> Self {
        Self {
            agent,
            app_name: None,
            session_service: None,
            user_id: "user".to_string(),
            session_id: None,
            mode: None,
            trust: false,
            console: None,
            thinking_mode: ThinkingDisplayMode::Auto,
            echo: true,
            rebuild: None,
            effort_rebuild: None,
            effort: None,
            status_lines: Vec::new(),
            banner_notes: Vec::new(),
            request_context_extractor: None,
            auth_token: None,
            governance: None,
            approval_service: None,
            background_runs: None,
            security_config: None,
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            run_store: None,
            goal_queue: None,
            kernel: None,
            hooks_disabled: false,
            opened: None,
            #[cfg(feature = "surface")]
            surface_records: None,
        }
    }

    /// Records that `--no-hooks` turned hooks off, so `/hooks` says so (spec 3.1 §7.22,
    /// §8.1). Build the harness with `HarnessAgentBuilder::without_hooks` as well.
    pub fn hooks_disabled(mut self, disabled: bool) -> Self {
        self.hooks_disabled = disabled;
        self
    }

    /// Sets the application name the console keeps sessions, modes and rules under.
    ///
    /// Defaults to the agent's name. The server always uses the agent's name, so name the
    /// agent after the app to share sessions between the console and the server.
    pub fn app_name(mut self, name: impl Into<String>) -> Self {
        self.app_name = Some(name.into());
        self
    }

    fn resolved_app_name(&self) -> String {
        self.app_name
            .clone()
            .unwrap_or_else(|| adk_core::Agent::name(self.agent.as_ref()).to_string())
    }

    /// Keeps sessions in `service`; use a persistent one to resume across processes.
    pub fn with_session_service(mut self, service: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(service);
        self
    }

    /// Sets the user the console acts for.
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    /// Continues the session `session_id`, creating it when it does not exist.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Switches the session to `mode` when it opens, as the person running the console
    /// (spec 3.1 §7.4). A loosening cancels the session's held calls first.
    pub fn with_mode(mut self, mode: HarnessMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Trusts the workspace's instruction files without asking (`--trust`, spec 3.1 §7.7).
    pub fn trust_workspace(mut self, trust: bool) -> Self {
        self.trust = trust;
        self
    }

    /// Uses `console` for reading lines and printing prompts instead of the terminal.
    pub fn with_console(mut self, console: ConsoleIo) -> Self {
        self.console = Some(console);
        self
    }

    /// Controls how emitted thinking content is rendered.
    pub fn with_thinking_mode(mut self, mode: ThinkingDisplayMode) -> Self {
        self.thinking_mode = mode;
        self
    }

    /// Prints nothing during [`run_once`](Self::run_once); the text is still returned.
    pub fn quiet(mut self) -> Self {
        self.echo = false;
        self
    }

    /// Lets `/sandbox on|off` rebuild the harness with the OS sandbox on or off.
    pub fn with_rebuild(mut self, rebuild: HarnessRebuild) -> Self {
        self.rebuild = Some(rebuild);
        self
    }

    /// Lets `/effort low|medium|high` rebuild the harness with every mode and role at that
    /// level (spec 3.1 §7.23).
    pub fn with_effort_rebuild(mut self, rebuild: HarnessEffortRebuild) -> Self {
        self.effort_rebuild = Some(rebuild);
        self
    }

    /// The effort `/effort` shows for the session; without it `/effort` shows "per mode".
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Adds a labelled line to the banner, such as the store or the effort.
    pub fn with_status_line(mut self, label: impl Into<String>, text: impl Into<String>) -> Self {
        self.status_lines.push((label.into(), text.into()));
        self
    }

    /// Adds a line to the banner, for example a setting a config file could not apply.
    pub fn with_banner_note(mut self, note: impl Into<String>) -> Self {
        self.banner_notes.push(note.into());
        self
    }

    /// Authenticates served requests with `extractor` (spec 3.1 §8.2).
    pub fn with_request_context_extractor(
        mut self,
        extractor: Arc<dyn RequestContextExtractor>,
    ) -> Self {
        self.request_context_extractor = Some(extractor);
        self
    }

    /// Admits served requests carrying `Authorization: Bearer <token>` as the operator: the
    /// launcher's [user](Self::with_user_id) with the [`OPERATOR_SCOPE`](super::OPERATOR_SCOPE)
    /// (spec 3.1 §8.2). Every caller is then the operator, so the web home's turns run as them,
    /// with their mode and standing rules (spec 3.3 §5.1).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_cli::harness::HarnessLauncher;
    ///
    /// # fn example(agent: adk_harness::HarnessAgent) -> adk_core::Result<()> {
    /// let app = HarnessLauncher::new(agent).with_user_id("ana").with_auth_token("s3cret").build_app()?;
    /// # let _ = app;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Shares `control` as the kill switch of everything served: the server's runs, the
    /// background runs built over it, and the web home's turns, so the home's **Stop all**
    /// stops them all (spec 3.3 §11 S-I10). A kernel's own control takes its place.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_cli::harness::HarnessLauncher;
    /// use adk_core::GovernanceControl;
    ///
    /// # fn example(agent: adk_harness::HarnessAgent) {
    /// let control = GovernanceControl::new();
    /// let launcher = HarnessLauncher::new(agent).with_governance(control.clone());
    /// # let _ = launcher;
    /// # }
    /// ```
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Serves the approval inbox for `service` under `/api/approvals` (spec 3.1 §8.2).
    /// Build the harness with the same service so held calls reach it.
    pub fn with_approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// Serves background runs from `state` under `/api/runs` (spec 3.1 §8.2).
    pub fn with_background_runs(mut self, state: BackgroundState) -> Self {
        self.background_runs = Some(state);
        self
    }

    /// Sets the server's host and origin checks and its request timeout. Without one the
    /// server uses `SecurityConfig::default()` with a 15-minute request timeout, since a
    /// harness turn runs commands.
    pub fn with_security_config(mut self, config: SecurityConfig) -> Self {
        self.security_config = Some(config);
        self
    }

    /// Sets the address serve mode listens on. Defaults to `127.0.0.1`.
    pub fn with_bind_address(mut self, address: IpAddr) -> Self {
        self.bind_address = address;
        self
    }

    /// Runs goals as durable runs in `store` (spec 3.1 §7.11, §8.1): a goal survives the
    /// process, parks on a held call and resumes at that call once it is decided. Without a
    /// run store a goal runs in this process only.
    pub fn with_run_store(mut self, store: Arc<dyn DurableRunStore>) -> Self {
        self.run_store = Some(store);
        self
    }

    /// Enqueues durable goals on `queue` instead of
    /// [`GOAL_QUEUE`](adk_harness::GOAL_QUEUE): `background` hands them to a running
    /// kernel's background worker (spec 3.2 §8.1, `goal` on a running kernel).
    pub fn with_goal_queue(mut self, queue: impl Into<String>) -> Self {
        self.goal_queue = Some(queue.into());
        self
    }

    /// Serves the harness as the resident agent kernel's HTTP surface (spec 3.2 §7.1,
    /// §7.11, §8.1).
    ///
    /// [`run_serve_directly`](Self::run_serve_directly) then starts `kernel` before it
    /// listens, mounts its control plane ([`adk_kernel::control::router`]) under
    /// `/api/kernel` behind the same extractor as the approvals inbox, shares the kernel's
    /// kill switch with the server, registers the kernel as the freeze delegate of
    /// `POST /api/admin/freeze` (spec 3.2 §10 S4), and on Ctrl-C or `SIGTERM` drains and
    /// stops the kernel. Build the kernel over the same run store, approval service and
    /// sessions as the launcher, and its background runs with the `interactive` queue and
    /// no embedded worker, so the kernel's workers execute every run.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let kernel = adk_kernel::Kernel::builder()
    ///     .run_store(runs.clone())
    ///     .agent("adk-rust", runner)
    ///     .build()?;
    /// HarnessLauncher::new(harness).with_run_store(runs).with_kernel(kernel).run_serve_directly(8080).await?;
    /// ```
    pub fn with_kernel(mut self, kernel: adk_kernel::Kernel) -> Self {
        self.kernel = Some(kernel);
        self
    }

    /// Records the web home's commands on `trail` and keeps how each answer was given in
    /// `answers` (spec 3.3 §11), such as an `adk_surface::AuditAnswerLog` over the signed audit
    /// log, so "You said yes in Telegram" survives a restart.
    #[cfg(feature = "surface")]
    pub fn with_surface_records(
        mut self,
        trail: adk_core::audit_log::AuditTrail,
        answers: Arc<dyn adk_surface::AnswerLog>,
    ) -> Self {
        self.surface_records = Some(crate::surface::SurfaceRecords { trail, answers });
        self
    }

    /// The session the launcher works in, once a turn or the REPL has opened it.
    pub fn session_id(&self) -> Option<&str> {
        self.opened.as_ref().map(|shared| shared.session.as_ref())
    }

    /// Runs the REPL, or the server for `serve [--port PORT] [--host ADDRESS]`.
    ///
    /// # Errors
    ///
    /// Returns the console's or the server's error.
    pub async fn run(self) -> Result<()> {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Option<Command>,
        }
        #[derive(clap::Subcommand)]
        enum Command {
            /// The harness REPL (default)
            Chat,
            /// Serve the harness with the approval inbox
            Serve {
                #[arg(long, default_value_t = 8080)]
                port: u16,
                #[arg(long)]
                host: Option<IpAddr>,
            },
            /// Run the harness as the agent kernel, in the foreground (spec 3.2 §8.1)
            Up {
                #[arg(long, default_value_t = 8080)]
                port: u16,
                #[arg(long)]
                host: Option<IpAddr>,
            },
        }
        match Cli::parse().command.unwrap_or(Command::Chat) {
            Command::Chat => self.run_console_directly().await,
            Command::Serve { port, host } => {
                let launcher = match host {
                    Some(host) => self.with_bind_address(host),
                    None => self,
                };
                launcher.run_serve_directly(port).await
            }
            Command::Up { port, host } => {
                let launcher = match host {
                    Some(host) => self.with_bind_address(host),
                    None => self,
                };
                launcher.resident()?.run_serve_directly(port).await
            }
        }
    }

    /// This launcher with a kernel over its own stores, for `up` from [`run`](Self::run):
    /// the launcher's run store or an in-memory one, its approval service and sessions, the
    /// harness runner as the `adk-rust`-style app target and as `harness.goal`, and its
    /// home under the user's config directory named after the app.
    fn resident(mut self) -> Result<Self> {
        if self.kernel.is_some() {
            return Ok(self);
        }
        let app_name = self.resolved_app_name();
        let sessions =
            self.session_service.clone().unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
        self.session_service = Some(sessions.clone());
        let runs: Arc<dyn DurableRunStore> =
            self.run_store.clone().unwrap_or_else(|| Arc::new(adk_core::InMemoryRunStore::new()));
        self.run_store = Some(runs.clone());
        let root: Arc<dyn Agent> = self.agent.clone();
        let runner = Runner::builder()
            .app_name(&app_name)
            .agent(root)
            .session_service(sessions)
            .approval_store(self.agent.runtime().approval_store().clone())
            .build()?;
        // The harness glue registers the runner, its services and the goal progress
        // (spec 3.2 §10 HK1).
        let harness = Harness::with_runner(self.agent.clone(), runner)?
            .with_run_store(runs.clone())
            .with_goal_queue(adk_kernel::QueueClass::BACKGROUND);
        let home = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(&app_name);
        // The socket at the home, as `adk-rust up` serves it; the defaults serve none.
        let config = adk_kernel::KernelConfig::default()
            .with_app_name(&app_name)
            .with_home(home)
            .with_control(adk_kernel::control::ControlPlaneConfig::default());
        let kernel = harness.kernel().config(config).build().map_err(AdkError::from)?;
        // Decisions go through the harness's service, which wakes the kernel's approval
        // sleepers.
        if let Some(service) = self.agent.runtime().approval_service() {
            self.approval_service = Some(service.clone());
        }
        if self.background_runs.is_none() {
            let (target, runner) = harness.goal_target();
            self.background_runs = Some(
                BackgroundState::new()
                    .with_run_store(runs)
                    .with_app_name(&app_name)
                    .with_agent(app_name, runner.clone())
                    .with_agent(target, runner)
                    .with_worker_config(
                        adk_server::background::DurableWorkerConfig::default()
                            .with_queue(adk_kernel::QueueClass::INTERACTIVE),
                    )
                    .with_embedded_worker(false),
            );
        }
        Ok(self.with_kernel(kernel))
    }

    pub(super) async fn open(&mut self) -> Result<Arc<Shared>> {
        if let Some(shared) = &self.opened {
            return Ok(shared.clone());
        }
        let console = match &self.console {
            Some(console) => console.clone(),
            None => ConsoleIo::terminal()?,
        };
        let sessions =
            self.session_service.clone().unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
        let workspace = canonical_root(&self.agent);
        let app_name = self.resolved_app_name();
        let session = open_session(
            sessions.as_ref(),
            &app_name,
            &self.user_id,
            self.session_id.as_deref(),
            &workspace,
        )
        .await?;
        let harness = harness_for(
            self.agent.clone(),
            &app_name,
            sessions.clone(),
            self.run_store.clone(),
            Served::default(),
        )?;
        let harness = match &self.goal_queue {
            Some(queue) => harness.with_goal_queue(queue.clone()),
            None => harness,
        };
        let user = UserId::new(self.user_id.clone())?;
        let session = SessionId::new(session)?;
        if let Some(mode) = self.mode
            && harness.mode(&user, &session).await? != mode
        {
            harness.set_mode(&user, &session, mode).await?;
        }
        let shared = Arc::new(Shared {
            harness: RwLock::new(Arc::new(harness)),
            app_name,
            user,
            session,
            workspace,
            sessions,
            console,
            thinking_mode: self.thinking_mode,
            rebuild: self.rebuild.clone(),
            effort_rebuild: self.effort_rebuild.clone(),
            effort: Mutex::new(self.effort),
            status_lines: self.status_lines.clone(),
            run_store: self.run_store.clone(),
            hooks_disabled: self.hooks_disabled,
        });
        shared.decide_trust(self.trust).await?;
        self.opened = Some(shared.clone());
        Ok(shared)
    }

    /// Runs one turn with `text` and reports how it ended (spec 3.1 §8.1).
    ///
    /// Calls the policy holds for a decision are returned in [`TurnOutcome::held`] when the
    /// harness has no prompter; decide them with `adk-rust approvals` and run again in the
    /// same session, which resumes them first.
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot be opened or the run cannot start.
    pub async fn run_once(&mut self, text: &str) -> Result<TurnOutcome> {
        let shared = self.open().await?;
        shared.turn(Some(text), self.echo).await
    }

    /// Closes the harness after [`run_once`](Self::run_once) turns: a session with turns not
    /// yet distilled is distilled into long-term memory first (spec 3.1 §7.15.2). A
    /// distillation that fails is reported on the console in one line.
    pub async fn close(&self) {
        if let Some(shared) = &self.opened {
            report_close(&shared.console, shared.harness().close_with_report().await);
        }
    }

    /// The todo list of the session [`run_once`](Self::run_once) works in (spec 3.1 §7.6);
    /// empty before the first turn.
    ///
    /// # Errors
    ///
    /// Returns the session service's error.
    pub async fn todos(&self) -> Result<adk_harness::TodoList> {
        match &self.opened {
            Some(shared) => shared.harness().todos(&shared.user, &shared.session).await,
            None => Ok(adk_harness::TodoList::default()),
        }
    }

    /// Runs the REPL until `exit` or end of input (spec 3.1 §8.1).
    ///
    /// # Errors
    ///
    /// Returns an error when the session cannot be opened or a run cannot start.
    pub async fn run_console_directly(mut self) -> Result<()> {
        let shared = self.open().await?;
        let commands = harness_commands(shared.clone());
        shared.print_banner(&self.banner_notes);
        // Tab completes the commands and, once the servers run, `@server:uri` mentions.
        shared.console.set_command_completions(command_completions(&commands));
        shared.refresh_mentions().await;
        let context = ConsoleCommandContext::new(
            shared.app_name.clone(),
            shared.user.to_string(),
            shared.session.to_string(),
            shared.sessions.clone(),
            shared.console.clone(),
        );
        if shared.has_held_calls().await? {
            shared.console.print("Resuming calls held in this session…\n");
            report(&shared.turn(None, true).await?);
        }

        let console = shared.console.clone();
        loop {
            let line = match console.read_line("\x1b[36mYou >\x1b[0m ").await {
                ConsoleInput::Line(line) => line,
                ConsoleInput::Interrupted => {
                    println!("\nInterrupted. Type exit to quit.\n");
                    continue;
                }
                ConsoleInput::Eof => {
                    println!("\nGoodbye.\n");
                    break;
                }
            };
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            if is_exit_command(&trimmed) {
                println!("\nGoodbye.\n");
                break;
            }
            console.add_history(&line);
            if trimmed == "/help" {
                print!("{}", help_text(&commands));
                continue;
            }
            if trimmed == "/clear" {
                println!("(conversation cleared — session state is unchanged)");
                continue;
            }
            if let Some((command, args)) = find_command(&commands, &trimmed) {
                match command.run(args, &context).await {
                    Ok(output) if output.is_empty() => {}
                    Ok(output) => println!("{}", terminal_safe(output.trim_end())),
                    Err(e) => eprintln!("Error: {}", terminal_safe(&e.to_string())),
                }
                continue;
            }
            println!();
            let text = shared.expand_mentions(&trimmed).await;
            report(&shared.turn(Some(&text), true).await?);
            // Workspace servers start with the first run, so their resources appear now.
            shared.refresh_mentions().await;
        }
        if memory_on(&shared.harness()) {
            println!("Saving what this session taught to long-term memory…");
        }
        report_close(&shared.console, shared.harness().close_with_report().await);
        Ok(())
    }

    /// Builds the HTTP app serving the harness (spec 3.1 §8.2).
    ///
    /// The harness is mounted through `ServerBuilder` with the approval inbox and
    /// background runs when configured, and its standing rules under `/api/rules`
    /// (spec 3.1 §7.5): list, create from a held call's suggestion or explicitly, revoke,
    /// for callers with the `operator` scope, each audited with `surface: rest`. Without an
    /// extractor, loopback peers are admitted
    /// as the operator ([`LoopbackOperatorExtractor`]), and the router records each
    /// request's peer for it; serve it with `into_make_service_with_connect_info`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the bind address is not loopback and no
    /// extractor authenticates requests.
    pub fn build_app(self) -> Result<Router> {
        let kernel = self.kernel.clone();
        // A paired device's token admits it, scoped, on every route (spec 3.3 §10 K2).
        let pairing = kernel.as_ref().and_then(|kernel| kernel.config().control.pairing.clone());
        let devices = |extractor: Arc<dyn RequestContextExtractor>| match &pairing {
            Some(pairing) => Arc::new(
                adk_kernel::pairing::DeviceTokenExtractor::new(pairing.clone())
                    .with_fallback(extractor),
            ) as Arc<dyn RequestContextExtractor>,
            None => extractor,
        };
        // One kill switch for everything served: the kernel's, else this launcher's, so the
        // home's Stop all reaches the server's runs and the background runs (spec 3.3 S-I10).
        let governance = match &kernel {
            Some(kernel) => kernel.config().governance.clone(),
            None => self.governance.clone().unwrap_or_default(),
        };
        let extractor = self.request_context_extractor.clone().or_else(|| {
            self.auth_token.as_ref().map(|token| {
                Arc::new(super::serve::BearerTokenExtractor::new(
                    token.clone(),
                    self.user_id.clone(),
                )) as Arc<dyn RequestContextExtractor>
            })
        });
        // The web home over this harness (spec 3.3 §5.1, §10 S1), on the sessions the server
        // serves, so both see one store. Its turns run as the operator when every caller is
        // (loopback or the token), so their mode and standing rules apply (spec 3.1 §8.2).
        #[cfg(feature = "surface")]
        let (surface, sessions, take_over) = {
            let app = self.resolved_app_name();
            let sessions = self
                .session_service
                .clone()
                .unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
            let operator = self
                .request_context_extractor
                .is_none()
                .then(|| super::serve::operator(&self.user_id));
            let harness = harness_for(
                self.agent.clone(),
                &app,
                sessions.clone(),
                self.run_store.clone(),
                Served { governance: Some(governance.clone()), caller: operator },
            )?;
            let engine = crate::surface::harness_engine(
                &app,
                Arc::new(harness),
                sessions.clone(),
                self.approval_service.clone(),
                kernel.clone(),
                self.surface_records.clone(),
            )?;
            (engine, sessions, self.agent.runtime().take_over().cloned())
        };
        let rules = super::rules::rules_router(super::rules::RuleEndpoints {
            agent: self.agent.clone(),
            app_name: self.resolved_app_name(),
            approvals: self.approval_service.clone(),
        });
        let mut launcher = Launcher::new(self.agent.agent())
            .app_name(self.resolved_app_name())
            .with_bind_address(self.bind_address)
            .with_api_routes(rules);
        // The kernel's control plane beside the inbox, behind the same extractor, and its
        // kill switch on the server (spec 3.2 §7.11, §10 S4).
        match kernel.clone() {
            Some(kernel) => {
                launcher = launcher
                    .with_api_routes(adk_kernel::control::router(kernel.clone()))
                    .with_kernel_governance(kernel.config().governance.clone(), Arc::new(kernel));
            }
            None => launcher = launcher.with_governance(governance),
        }
        #[cfg(feature = "surface")]
        {
            // Take over, then Done: a secret typed on the home reaches the call that asked
            // (spec 3.3 §8.2, §10 H2).
            if let Some(take_over) = take_over {
                launcher = launcher.with_api_routes(adk_surface::web::take_over_router(
                    surface.clone(),
                    take_over,
                ));
            }
            launcher = launcher.with_session_service(sessions).with_surface(surface);
        }
        #[cfg(not(feature = "surface"))]
        if let Some(sessions) = self.session_service {
            launcher = launcher.with_session_service(sessions);
        }
        let security = self.security_config.unwrap_or_else(|| SecurityConfig {
            request_timeout: SERVE_REQUEST_TIMEOUT,
            ..SecurityConfig::default()
        });
        launcher = launcher.with_security_config(security);
        if let Some(service) = self.approval_service {
            launcher = launcher.with_approval_service(service);
        }
        if let Some(state) = self.background_runs {
            launcher = launcher.with_background_runs(state);
        }
        let loopback = match extractor {
            Some(extractor) => {
                launcher = launcher.with_request_context_extractor(devices(extractor));
                None
            }
            None if self.bind_address.is_loopback() => {
                let extractor = LoopbackOperatorExtractor::new(self.user_id.clone());
                launcher =
                    launcher.with_request_context_extractor(devices(Arc::new(extractor.clone())));
                Some(extractor)
            }
            None => {
                return Err(AdkError::config(format!(
                    "serving the harness on {} needs authentication: pass --auth-token or set \
                     ADK_SERVE_TOKEN, or listen on 127.0.0.1",
                    self.bind_address
                )));
            }
        };
        // The claim route needs no caller but keeps the server's host and origin checks.
        if let (Some(kernel), Some(_)) = (&kernel, &pairing) {
            launcher = launcher.with_root_routes(adk_kernel::control::claim_router(kernel.clone()));
        }
        let router = launcher.build_app()?;
        // Every route checks a device token first.
        let router = match kernel {
            Some(kernel) if pairing.is_some() => {
                adk_kernel::control::guard_devices(&kernel, router)
            }
            _ => router,
        };
        Ok(match loopback {
            Some(extractor) => extractor.layer(router),
            None => router,
        })
    }

    /// Serves the harness on the bind address and `port` until Ctrl-C (spec 3.1 §8.2).
    ///
    /// # Errors
    ///
    /// Returns the [`build_app`](Self::build_app) error, or the listener's.
    pub async fn run_serve_directly(self, port: u16) -> Result<()> {
        let addr = SocketAddr::new(self.bind_address, port);
        let info = self.agent.info().clone();
        let kernel = self.kernel.clone();
        let app = self.build_app()?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| AdkError::config(format!("failed to bind {addr}: {e}")))?;
        if let Some(kernel) = kernel {
            return serve_resident(kernel, listener, app, addr, &info).await;
        }
        println!("ADK-Rust harness serving on http://{addr}");
        println!("  mode {} · shell {}", info.default_mode, terminal_safe(&shell_label(&info)));
        #[cfg(feature = "surface")]
        println!("  web home  http://{addr}/ui/");
        println!("  approvals: GET /api/approvals, POST /api/approvals/{{id}}/decide");
        println!("  standing rules: GET /api/rules, POST /api/rules, DELETE /api/rules/{{id}}");
        println!("Press Ctrl+C to stop\n");
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| AdkError::config(format!("server error: {e}")))
    }
}

/// Where `bash` runs, or `none` when the harness offers no shell (spec 3.3 §10 H1).
pub(crate) fn shell_label(info: &adk_harness::HarnessInfo) -> String {
    info.shell().map_or_else(|| "none".to_string(), ToString::to_string)
}

/// Serves `app` on `listener` with `kernel` resident: the kernel starts before the first
/// request and drains and stops on Ctrl-C, `SIGTERM` or a Windows service stop (spec 3.2
/// §7.2, §8.1, §10 W3).
async fn serve_resident(
    kernel: adk_kernel::Kernel,
    listener: tokio::net::TcpListener,
    app: Router,
    addr: SocketAddr,
    info: &adk_harness::HarnessInfo,
) -> Result<()> {
    kernel.start().await.map_err(AdkError::from)?;
    let config = kernel.config();
    println!("ADK-Rust kernel up on http://{addr}");
    #[cfg(windows)]
    if let Some(pipe) = config.control.pipe_name(&config.home) {
        println!("  pipe {pipe}");
    }
    #[cfg(not(windows))]
    if let Some(socket) = config.control.socket_path(&config.home) {
        println!("  socket {}", socket.display());
    }
    println!("  home {}", config.home.display());
    println!("  mode {} · shell {}", info.default_mode, terminal_safe(&shell_label(info)));
    let queues: Vec<&str> = config.queues.iter().map(adk_kernel::QueueClass::as_str).collect();
    println!("  queues {}", queues.join(", "));
    println!(
        "  control: adk-rust ps | top | attach | kill | suspend | resume | freeze | events | proc"
    );
    #[cfg(feature = "surface")]
    println!("  web home  http://{addr}/ui/");
    println!("  approvals: GET /api/approvals, POST /api/approvals/{{id}}/decide");
    println!("Press Ctrl+C to drain and stop\n");
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                // A Windows service is stopped by its manager, not by a signal (spec 3.2
                // §10 W3); elsewhere `stop_requested` never resolves.
                tokio::select! {
                    () = shutdown_signal() => {}
                    () = adk_kernel::service::stop_requested() => {}
                }
            })
            .await
    });
    // Both wait for the same stop: the server stops accepting, the kernel drains.
    let stopped = kernel.shutdown_on_signal().await.map_err(AdkError::from);
    let served = server.await.map_err(|e| AdkError::config(format!("server task failed: {e}")))?;
    served.map_err(|e| AdkError::config(format!("server error: {e}")))?;
    stopped
}

/// Prints how a REPL turn ended.
fn report(outcome: &TurnOutcome) {
    if outcome.ended_by_denial {
        println!("\nTurn ended: you denied a call. Say what to do instead.\n");
    } else if outcome.interrupted {
        println!("\nInterrupted.\n");
    } else if !outcome.held.is_empty() {
        println!(
            "\n{} call(s) are held for a decision: `adk-rust approvals list`, then continue \
             this session.\n",
            outcome.held.len()
        );
    } else {
        println!("\n");
    }
}

/// Prints one line per session whose turns were not distilled into long-term memory
/// (spec 3.1 §7.15.2), so a failed distillation is not silent.
fn report_close(console: &ConsoleIo, report: adk_harness::CloseReport) {
    for (session, error) in &report.distillation_failures {
        console.print(&format!(
            "note: long-term memory was not updated for session {}: {error}\n",
            adk_harness::escape_control(session)
        ));
    }
}

/// Whether the harness keeps long-term facts (spec 3.1 §7.15.2).
pub(crate) fn memory_on(harness: &Harness) -> bool {
    harness.agent().info().tools.iter().any(|tool| tool == "memory_remember")
}

/// A `Harness` whose runner asks the agent's prompter and waits an hour for a person, and
/// that runs goals as durable runs in `run_store` when one is given.
/// What a served harness's runner shares with the server: its kill switch, and the caller its
/// turns run as when every caller is the operator.
#[derive(Default)]
pub(crate) struct Served {
    pub(crate) governance: Option<GovernanceControl>,
    pub(crate) caller: Option<adk_core::RequestContext>,
}

pub(crate) fn harness_for(
    agent: Arc<HarnessAgent>,
    app_name: &str,
    sessions: Arc<dyn SessionService>,
    run_store: Option<Arc<dyn DurableRunStore>>,
    served: Served,
) -> Result<Harness> {
    let runtime = agent.runtime();
    let config = RunConfig {
        tool_confirmation_handler: runtime.confirmation_handler().cloned(),
        tool_confirmation_timeout: CONSOLE_CONFIRMATION_TIMEOUT,
        ..RunConfig::default()
    };
    let root: Arc<dyn Agent> = agent.clone();
    let mut builder = Runner::builder()
        .app_name(app_name)
        .agent(root)
        .session_service(sessions)
        .run_config(config)
        .approval_store(runtime.approval_store().clone());
    if let Some(compaction) = runtime.compaction() {
        builder = builder.compaction_config(compaction.clone());
    }
    if let Some(governance) = served.governance {
        builder = builder.governance(governance);
    }
    if let Some(caller) = served.caller {
        builder = builder.request_context(caller);
    }
    let harness = Harness::with_runner(agent, builder.build()?)?;
    // Goals get their own queue, so this worker and a server's background worker on the
    // same store never take each other's runs.
    Ok(match run_store {
        Some(store) => harness.with_run_store(store).with_goal_queue(adk_harness::GOAL_QUEUE),
        None => harness,
    })
}

fn canonical_root(agent: &HarnessAgent) -> PathBuf {
    let root = agent.runtime().workspace().root();
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Opens `requested` or a new session, recording the workspace in a new one and refusing
/// one that works in another workspace (spec 3.1 §8.1).
pub(crate) async fn open_session(
    sessions: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    requested: Option<&str>,
    workspace: &Path,
) -> Result<String> {
    let recorded = workspace.display().to_string();
    if let Some(session_id) = requested {
        let existing = sessions
            .get(GetRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await;
        match existing {
            Ok(session) => {
                if let Some(other) = session.state().get(WORKSPACE_STATE_KEY)
                    && other.as_str() != Some(recorded.as_str())
                {
                    let other = other.as_str().unwrap_or_default().to_string();
                    return Err(AdkError::config(format!(
                        "session {session_id} works in {other}, not {recorded}; continue it with \
                         --dir {other}, or start a new session here"
                    )));
                }
                return Ok(session.id().to_string());
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }
    }
    let created = sessions
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: requested.map(str::to_string),
            state: HashMap::from([(WORKSPACE_STATE_KEY.to_string(), json!(recorded))]),
        })
        .await?;
    Ok(created.id().to_string())
}

/// What the REPL, its commands and one-turn runs share.
pub(crate) struct Shared {
    harness: RwLock<Arc<Harness>>,
    pub(crate) app_name: String,
    pub(crate) user: UserId,
    pub(crate) session: SessionId,
    pub(crate) workspace: PathBuf,
    pub(crate) sessions: Arc<dyn SessionService>,
    pub(crate) console: ConsoleIo,
    thinking_mode: ThinkingDisplayMode,
    rebuild: Option<HarnessRebuild>,
    effort_rebuild: Option<HarnessEffortRebuild>,
    effort: Mutex<Option<Effort>>,
    status_lines: Vec<(String, String)>,
    pub(crate) run_store: Option<Arc<dyn DurableRunStore>>,
    /// `--no-hooks` turned hooks off.
    pub(crate) hooks_disabled: bool,
}

impl Shared {
    /// Replaces the `@server:uri` mentions Tab completes with what every MCP server
    /// publishes (spec 3.1 §7.21); a server that cannot be listed — not started yet, or
    /// failing — offers none.
    pub(crate) async fn refresh_mentions(&self) {
        let harness = self.harness();
        let runtime = harness.agent().runtime();
        let Some(mcp) = runtime.mcp() else {
            return;
        };
        let mut mentions = Vec::new();
        for (server, _) in mcp.servers() {
            let Ok(listed) = mcp.list_resources(&server).await else {
                continue;
            };
            for resource in listed["resources"].as_array().into_iter().flatten() {
                if let Some(uri) = resource["uri"].as_str() {
                    mentions.push(format!("@{server}:{uri}"));
                }
            }
        }
        self.console.set_mention_completions(mentions);
    }

    /// The harness turns run on now.
    pub(crate) fn harness(&self) -> Arc<Harness> {
        self.harness.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Replaces the harness with one built with the sandbox on or off.
    pub(crate) fn rebuild(&self, sandbox: bool) -> Result<Option<Arc<Harness>>> {
        let Some(rebuild) = &self.rebuild else {
            return Ok(None);
        };
        let agent = Arc::new(rebuild(sandbox)?);
        let harness = Arc::new(harness_for(
            agent,
            &self.app_name,
            self.sessions.clone(),
            self.run_store.clone(),
            Served::default(),
        )?);
        *self.harness.write().unwrap_or_else(std::sync::PoisonError::into_inner) = harness.clone();
        Ok(Some(harness))
    }

    /// Expands `@server:uri` mentions: each resource the harness can read is appended to
    /// the turn inside its untrusted block, so a human hands a resource to the model without
    /// a tool round (spec 3.1 §7.21, I52). A mention that cannot be read is reported and left
    /// as typed.
    pub(crate) async fn expand_mentions(&self, text: &str) -> String {
        let harness = self.harness();
        let runtime = harness.agent().runtime();
        if runtime.mcp().is_none() {
            return text.to_string();
        }
        let mut out = text.to_string();
        for word in text.split_whitespace() {
            let Some(mention) = word.strip_prefix('@') else { continue };
            let Some((server, uri)) = mention.split_once(':') else { continue };
            if server.is_empty() || uri.is_empty() {
                continue;
            }
            match runtime
                .mcp_resource_mention(
                    &self.app_name,
                    self.user.as_str(),
                    self.session.as_str(),
                    server,
                    uri,
                )
                .await
            {
                Ok(resource) => {
                    if let Some(block) = resource["content"].as_str() {
                        out.push_str("\n\n");
                        out.push_str(block);
                    }
                }
                Err(error) => {
                    println!("(@{server}:{uri} not read: {})", terminal_safe(&error.to_string()))
                }
            }
        }
        out
    }

    /// Replaces the harness with one whose every mode and role runs at `effort`
    /// (spec 3.1 §7.23).
    pub(crate) async fn rebuild_effort(&self, effort: Effort) -> Result<Option<Arc<Harness>>> {
        let Some(rebuild) = &self.effort_rebuild else {
            return Ok(None);
        };
        let agent = Arc::new(rebuild(effort).await?);
        let harness = Arc::new(harness_for(
            agent,
            &self.app_name,
            self.sessions.clone(),
            self.run_store.clone(),
            Served::default(),
        )?);
        *self.harness.write().unwrap_or_else(std::sync::PoisonError::into_inner) = harness.clone();
        *self.effort.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(effort);
        Ok(Some(harness))
    }

    /// The session's effort, once `/effort` or the surface set one.
    pub(crate) fn current_effort(&self) -> Option<Effort> {
        *self.effort.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The session's state value at `key`.
    pub(crate) async fn state(&self, key: &str) -> Result<Option<Value>> {
        let session = self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: self.user.to_string(),
                session_id: self.session.to_string(),
                num_recent_events: Some(1),
                after: None,
            })
            .await?;
        Ok(session.state().get(key))
    }

    /// Every event of the session.
    pub(crate) async fn events(&self) -> Result<Vec<Event>> {
        Ok(self
            .sessions
            .get(GetRequest {
                app_name: self.app_name.clone(),
                user_id: self.user.to_string(),
                session_id: self.session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await?
            .events()
            .all())
    }

    async fn has_held_calls(&self) -> Result<bool> {
        let events = self.events().await?;
        let mut holders = Vec::<(&str, &str)>::new();
        for event in &events {
            if event.actions.tool_confirmation.is_some()
                && !holders.contains(&(event.author.as_str(), event.branch.as_str()))
            {
                holders.push((event.author.as_str(), event.branch.as_str()));
            }
        }
        Ok(holders
            .iter()
            .any(|(author, branch)| !held_tool_calls(&events, author, branch).is_empty()))
    }

    /// Runs one turn: `text`, or an empty turn that resumes held calls.
    pub(crate) async fn turn(&self, text: Option<&str>, echo: bool) -> Result<TurnOutcome> {
        let harness = self.harness();
        let token = CancellationToken::new();
        self.console.set_run(Some(token.clone()));
        let started = match text {
            Some(text) => harness.send(&self.user, &self.session, text).await,
            None => harness.resume(&self.user, &self.session).await,
        };
        self.drive(started, token, echo).await
    }

    /// Prints a started run's events until it ends; Ctrl-C and a person's denial cancel
    /// `token`, which interrupts the session's run.
    pub(crate) async fn drive(
        &self,
        started: Result<adk_core::EventStream>,
        token: CancellationToken,
        echo: bool,
    ) -> Result<TurnOutcome> {
        let harness = self.harness();
        let events = match started {
            Ok(events) => events,
            Err(error) => {
                self.console.set_run(None);
                return Err(error);
            }
        };
        // Ctrl-C and a person's denial cancel the token; the runner stops the session's run.
        let watcher = tokio::spawn({
            let harness = harness.clone();
            let token = token.clone();
            let session = self.session.clone();
            async move {
                token.cancelled().await;
                harness.interrupt(&session);
            }
        });
        let end = drive_turn(events, &self.console, &token, self.thinking_mode, echo).await;
        watcher.abort();
        self.console.set_run(None);
        Ok(TurnOutcome {
            session_id: self.session.to_string(),
            held: end.held,
            interrupted: end.interrupted,
            ended_by_denial: end.ended_by_denial,
            text: end.text,
        })
    }

    /// Asks once whether to trust the workspace's instruction files (spec 3.1 §7.7).
    async fn decide_trust(&self, trust: bool) -> Result<()> {
        let harness = self.harness();
        let runtime = harness.agent().runtime();
        let instructions = runtime.instructions();
        if runtime.workspace_trusted() || instructions.project_files().is_empty() {
            return Ok(());
        }
        if trust {
            return harness.trust_workspace(&self.user, TrustDecision::Trusted).await;
        }
        if !self.console.is_terminal() {
            self.console.print(
                "Instruction files in this workspace are not loaded: it is not trusted. Pass \
                 --trust to load them.\n",
            );
            return Ok(());
        }
        let mut text =
            String::from("\nThis workspace has instruction files the agent would follow:\n");
        for file in instructions.project_files() {
            text.push_str(&format!(
                "  {}  {} bytes  sha256 {}\n",
                terminal_safe(&file.logical_path),
                file.bytes,
                &file.sha256[..file.sha256.len().min(12)]
            ));
        }
        text.push_str("They cannot grant permissions; they shape what the agent tries.\n");
        // A trusted workspace also runs its hooks, so the prompt lists each with its
        // command (spec 3.1 §7.22, §9.2 I54).
        let hooks = runtime
            .hooks()
            .map(|set| set.describe(adk_harness::hooks::HookSource::Workspace))
            .unwrap_or_default();
        if !hooks.is_empty() {
            text.push_str(&format!(
                "Hooks in {} that would run on tool events, as the shell runs:\n",
                adk_harness::hooks::WORKSPACE_HOOKS_FILE
            ));
            for hook in &hooks {
                text.push_str(&format!("  {}\n", terminal_safe(hook)));
            }
        }
        self.console.print(&text);
        let decision = match self.console.read_line("Trust them? [y/N] > ").await {
            ConsoleInput::Line(answer) if matches!(answer.trim(), "y" | "yes") => {
                TrustDecision::Trusted
            }
            _ => TrustDecision::Untrusted,
        };
        harness.trust_workspace(&self.user, decision).await
    }

    fn print_banner(&self, notes: &[String]) {
        let harness = self.harness();
        let info = harness.agent().info();
        let version = env!("CARGO_PKG_VERSION");
        println!();
        println!("  ADK-Rust harness v{version}");
        println!("  Workspace : {}", terminal_safe(&self.workspace.display().to_string()));
        println!("  Mode      : {}", info.default_mode);
        let isolation = match &info.degraded_reason {
            Some(reason) => format!("{} ({})", shell_label(info), reason),
            None => shell_label(info),
        };
        println!("  Shell     : {}", terminal_safe(&isolation));
        let members =
            if info.members.is_empty() { "none".to_string() } else { info.members.join(", ") };
        println!("  Members   : {}", terminal_safe(&members));
        for (label, text) in &self.status_lines {
            println!("  {:<10}: {}", terminal_safe(label), terminal_safe(text));
        }
        if memory_on(&harness) {
            println!("  Memory    : long-term facts on (/memory)");
        }
        println!("  Session   : {}", terminal_safe(self.session.as_ref()));
        for note in notes {
            println!("  Note      : {}", terminal_safe(note));
        }
        println!();
        println!("  Type a task. /help for commands, /mode to change the mode.");
        println!();
    }
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("session", &self.session).finish_non_exhaustive()
    }
}
