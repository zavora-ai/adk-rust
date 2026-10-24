//! Simple launcher for ADK agents with CLI support.
//!
//! Provides a one-liner to run agents with console or web server modes,
//! similar to adk-go's launcher pattern.
//!
//! # Example
//!
//! ```ignore
//! use adk_cli::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> adk_core::Result<()> {
//!     let agent = /* create your agent */;
//!
//!     // Run with CLI support (console by default, or `serve` for web)
//!     Launcher::new(Arc::new(agent)).run().await
//! }
//! ```
//!
//! # CLI Usage
//!
//! ```bash
//! # Interactive console (default)
//! cargo run
//!
//! # Web server (the REST API under /api), on loopback
//! cargo run -- serve
//! cargo run -- serve --port 3000
//!
//! # Listen on every interface (configure authentication and allowed hosts first)
//! cargo run -- serve --host 0.0.0.0
//! ```

use crate::console::{
    ConsoleCommand, ConsoleCommandContext, ConsoleConfirmationHandler, ConsoleInput, ConsoleIo,
};
use adk_artifact::ArtifactService;
use adk_core::{
    AdkError, Agent, ApprovalService, CacheCapable, Content, ContextCacheConfig,
    DEFAULT_TOOL_CONFIRMATION_TIMEOUT, Event, EventsCompactionConfig, Memory, Part, Result,
    RunConfig, SessionId, StreamingMode, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, ToolPolicy, UserId,
};
use adk_runner::Runner;
use adk_server::background::BackgroundState;
use adk_server::{
    RequestContextExtractor, SecurityConfig, ServerBuilder, ServerConfig, shutdown_signal,
};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use axum::Router;
use clap::{Parser, Subcommand};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

/// How long the console waits for a person to answer an approval prompt when the run
/// configuration keeps [`DEFAULT_TOOL_CONFIRMATION_TIMEOUT`] (spec 3.1 §8.1).
pub const CONSOLE_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// How often serve mode expires and escalates approval requests.
const APPROVAL_TICK_INTERVAL: Duration = Duration::from_secs(30);

/// CLI arguments for the launcher.
#[derive(Parser)]
#[command(name = "agent")]
#[command(about = "ADK Agent", long_about = None)]
struct LauncherCli {
    #[command(subcommand)]
    command: Option<LauncherCommand>,
}

#[derive(Subcommand)]
enum LauncherCommand {
    /// Run interactive console (default if no command specified)
    Chat,
    /// Start web server with UI
    Serve {
        /// Server port
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Address to listen on. Defaults to the launcher's bind address (127.0.0.1
        /// unless `with_bind_address` changed it); pass 0.0.0.0 to listen on every
        /// interface.
        #[arg(long)]
        host: Option<IpAddr>,
    },
    /// Optimize agent instructions using an evaluation set
    #[cfg(feature = "optimize")]
    Optimize(crate::optimize::OptimizeArgs),
}

/// Controls how the console renders thinking/reasoning content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingDisplayMode {
    /// Show thinking when the model emits it.
    #[default]
    Auto,
    /// Always surface emitted thinking.
    Show,
    /// Hide emitted thinking from the console output.
    Hide,
}

/// Controls how serve mode initializes telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelemetryConfig {
    /// Use the in-memory ADK span exporter for debug endpoints.
    AdkExporter { service_name: String },
    /// Export telemetry to an OTLP collector.
    Otlp { service_name: String, endpoint: String },
    /// Disable telemetry initialization in the launcher.
    None,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self::AdkExporter { service_name: "adk-server".to_string() }
    }
}

/// Launcher for running ADK agents with CLI support.
///
/// Provides console and web server modes out of the box.
///
/// # Console mode
///
/// Uses a `rustyline` REPL with history, Ctrl+C handling, and streaming
/// output that renders `<think>` blocks, tool calls, and inline data.
///
/// # Serve mode
///
/// Starts an Axum HTTP server with the ADK web UI on `127.0.0.1`. Listening on
/// other interfaces is an explicit choice: [`with_bind_address`](Self::with_bind_address)
/// or `serve --host`. The server then answers only loopback names and IP addresses until
/// its public host names are listed in [`SecurityConfig::allowed_hosts`].
///
/// # Console runs and serve mode
///
/// [`with_run_config`](Self::with_run_config), [`with_streaming_mode`](Self::with_streaming_mode),
/// [`with_tool_policy`](Self::with_tool_policy) and
/// [`with_confirmation_handler`](Self::with_confirmation_handler) configure console runs.
/// `ServerConfig` does not accept a `RunConfig`, so serve mode does not apply them, and
/// [`build_app`](Self::build_app) refuses to serve when a tool policy is set rather than
/// serve without it. [`with_approval_service`](Self::with_approval_service) and
/// [`with_background_runs`](Self::with_background_runs) configure serve mode.
pub struct Launcher {
    agent: Arc<dyn Agent>,
    app_name: Option<String>,
    session_service: Option<Arc<dyn SessionService>>,
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn Memory>>,
    compaction_config: Option<EventsCompactionConfig>,
    context_cache_config: Option<ContextCacheConfig>,
    cache_capable: Option<Arc<dyn CacheCapable>>,
    security_config: Option<SecurityConfig>,
    request_context_extractor: Option<Arc<dyn RequestContextExtractor>>,
    a2a_base_url: Option<String>,
    telemetry_config: TelemetryConfig,
    shutdown_grace_period: Duration,
    run_config: Option<RunConfig>,
    streaming_mode: Option<StreamingMode>,
    tool_policy: Option<Arc<dyn ToolPolicy>>,
    confirmation_handler: Option<Arc<dyn ToolConfirmationHandler>>,
    user_id: Option<String>,
    session_id: Option<String>,
    console_commands: Vec<Arc<dyn ConsoleCommand>>,
    approval_service: Option<Arc<dyn ApprovalService>>,
    background_runs: Option<BackgroundState>,
    /// Routes mounted under `/api` behind the auth middleware, such as a served harness's
    /// standing-rule routes.
    api_routes: Vec<Router>,
    /// Routes merged at the root inside the server's middleware but outside its auth
    /// middleware, such as a kernel's device pairing claim route.
    root_routes: Vec<Router>,
    /// The kill switch the server shares and the delegate its admin freeze calls: a
    /// resident harness's kernel (spec 3.2 §7.2, §10 S4).
    #[cfg(feature = "harness")]
    kernel_governance:
        Option<(adk_core::GovernanceControl, Arc<dyn adk_server::rest::FreezeDelegate>)>,
    /// The kill switch shared with a served harness (spec 3.3 §11 S-I10).
    #[cfg(feature = "harness")]
    governance: Option<adk_core::GovernanceControl>,
    /// The web home's engine, which a served harness mounts (spec 3.3 §5.1, §10 S1).
    #[cfg(all(feature = "surface", feature = "harness"))]
    surface: Option<adk_surface::SurfaceEngine>,
    thinking_mode: ThinkingDisplayMode,
    bind_address: IpAddr,
}

impl Launcher {
    /// Create a new launcher with the given agent.
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self {
            agent,
            app_name: None,
            session_service: None,
            artifact_service: None,
            memory_service: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            security_config: None,
            request_context_extractor: None,
            a2a_base_url: None,
            telemetry_config: TelemetryConfig::default(),
            shutdown_grace_period: Duration::from_secs(30),
            run_config: None,
            streaming_mode: None,
            tool_policy: None,
            confirmation_handler: None,
            user_id: None,
            session_id: None,
            console_commands: Vec::new(),
            approval_service: None,
            background_runs: None,
            api_routes: Vec::new(),
            root_routes: Vec::new(),
            #[cfg(feature = "harness")]
            kernel_governance: None,
            #[cfg(feature = "harness")]
            governance: None,
            #[cfg(all(feature = "surface", feature = "harness"))]
            surface: None,
            thinking_mode: ThinkingDisplayMode::Auto,
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
        }
    }

    /// Set a custom application name (defaults to agent name).
    pub fn app_name(mut self, name: impl Into<String>) -> Self {
        self.app_name = Some(name.into());
        self
    }

    /// Set a custom artifact service.
    pub fn with_artifact_service(mut self, service: Arc<dyn ArtifactService>) -> Self {
        self.artifact_service = Some(service);
        self
    }

    /// Set a custom session service.
    pub fn with_session_service(mut self, service: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(service);
        self
    }

    /// Set a custom memory service.
    pub fn with_memory_service(mut self, service: Arc<dyn Memory>) -> Self {
        self.memory_service = Some(service);
        self
    }

    /// Enable runner-level context compaction in serve mode.
    pub fn with_compaction(mut self, config: EventsCompactionConfig) -> Self {
        self.compaction_config = Some(config);
        self
    }

    /// Enable automatic prompt cache lifecycle management in serve mode.
    pub fn with_context_cache(
        mut self,
        config: ContextCacheConfig,
        cache_capable: Arc<dyn CacheCapable>,
    ) -> Self {
        self.context_cache_config = Some(config);
        self.cache_capable = Some(cache_capable);
        self
    }

    /// Set the address serve mode listens on. Defaults to `127.0.0.1`.
    ///
    /// Pass `0.0.0.0` (or `::`) to accept connections on every interface. Configure
    /// authentication with [`with_request_context_extractor`](Self::with_request_context_extractor)
    /// first, and list the public host names in [`SecurityConfig::allowed_hosts`]
    /// through [`with_security_config`](Self::with_security_config).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::net::{IpAddr, Ipv4Addr};
    ///
    /// Launcher::new(agent)
    ///     .with_bind_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    ///     .with_security_config(SecurityConfig::default().with_allowed_hosts(["agents.example.com"]))
    ///     .run_serve_directly(8080)
    ///     .await?;
    /// ```
    pub fn with_bind_address(mut self, address: IpAddr) -> Self {
        self.bind_address = address;
        self
    }

    /// Set custom server security settings.
    pub fn with_security_config(mut self, config: SecurityConfig) -> Self {
        self.security_config = Some(config);
        self
    }

    /// Set a request context extractor for authenticated deployments.
    pub fn with_request_context_extractor(
        mut self,
        extractor: Arc<dyn RequestContextExtractor>,
    ) -> Self {
        self.request_context_extractor = Some(extractor);
        self
    }

    /// Enable A2A routes when building or serving the HTTP app.
    pub fn with_a2a_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.a2a_base_url = Some(base_url.into());
        self
    }

    /// Configure how serve mode initializes telemetry.
    pub fn with_telemetry(mut self, config: TelemetryConfig) -> Self {
        self.telemetry_config = config;
        self
    }

    /// Set the maximum graceful shutdown window for the web server.
    pub fn with_shutdown_grace_period(mut self, grace_period: Duration) -> Self {
        self.shutdown_grace_period = grace_period;
        self
    }

    /// Set streaming mode for console mode.
    ///
    /// It takes precedence over the streaming mode of
    /// [`with_run_config`](Self::with_run_config), whichever is called first. Note: this
    /// currently only affects console mode. The server does not yet accept a `RunConfig`.
    pub fn with_streaming_mode(mut self, mode: StreamingMode) -> Self {
        self.streaming_mode = Some(mode);
        self
    }

    /// Set the run configuration for console runs (spec 3.1 §8.1).
    ///
    /// The console merges it with its own settings: the streaming mode, tool policy and
    /// confirmation handler set through [`with_streaming_mode`](Self::with_streaming_mode),
    /// [`with_tool_policy`](Self::with_tool_policy) and
    /// [`with_confirmation_handler`](Self::with_confirmation_handler) take precedence over
    /// the same fields here, whichever is called first. A configuration without a
    /// confirmation handler gets a [`ConsoleConfirmationHandler`], and the default
    /// confirmation timeout is raised to [`CONSOLE_CONFIRMATION_TIMEOUT`]. Serve mode does
    /// not apply it.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_core::{Agent, RunBudget, RunConfig, StreamingMode};
    ///
    /// async fn launch(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    ///     Launcher::new(agent)
    ///         .with_run_config(RunConfig::builder().budget(RunBudget::new().max_model_calls(50)).build())
    ///         .with_streaming_mode(StreamingMode::None)
    ///         .run_console_directly()
    ///         .await
    /// }
    /// ```
    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = Some(config);
        self
    }

    /// Set the policy evaluated for every tool call of a console run (spec 3.1 §8.1).
    ///
    /// It replaces the policy of [`with_run_config`](Self::with_run_config). Serve mode
    /// cannot apply it, so [`build_app`](Self::build_app) fails while one is set.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_core::{Agent, DeclarativePolicy};
    ///
    /// async fn launch(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    ///     let policy = DeclarativePolicy::builder()
    ///         .allow_read_only()
    ///         .require_approval("*", "every other tool needs approval")
    ///         .build();
    ///     Launcher::new(agent).with_tool_policy(Arc::new(policy)).run_console_directly().await
    /// }
    /// ```
    pub fn with_tool_policy(mut self, policy: Arc<dyn ToolPolicy>) -> Self {
        self.tool_policy = Some(policy);
        self
    }

    /// Set what decides tool calls held for confirmation in console runs
    /// (spec 3.1 §8.1).
    ///
    /// Without one the console asks at the terminal through a
    /// [`ConsoleConfirmationHandler`], unless [`with_run_config`](Self::with_run_config)
    /// carries a handler.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_cli::console::{ConsoleConfirmationHandler, ConsoleIo};
    /// use adk_core::Agent;
    ///
    /// async fn launch(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    ///     let handler = ConsoleConfirmationHandler::new(ConsoleIo::terminal()?);
    ///     Launcher::new(agent).with_confirmation_handler(Arc::new(handler)).run_console_directly().await
    /// }
    /// ```
    pub fn with_confirmation_handler(mut self, handler: Arc<dyn ToolConfirmationHandler>) -> Self {
        self.confirmation_handler = Some(handler);
        self
    }

    /// Set the user console runs act for (spec 3.1 §8.1). Defaults to `"user"`.
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Continue the console session `session_id`, creating it when it does not exist
    /// (spec 3.1 §8.1).
    ///
    /// Without one the console starts a new session. Pair it with a persistent
    /// [`with_session_service`](Self::with_session_service) to resume a conversation
    /// across processes.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_core::Agent;
    /// use adk_session::SessionService;
    ///
    /// async fn resume(agent: Arc<dyn Agent>, sessions: Arc<dyn SessionService>) -> adk_core::Result<()> {
    ///     Launcher::new(agent)
    ///         .with_session_service(sessions)
    ///         .with_user_id("ana")
    ///         .with_session_id("release-notes")
    ///         .run_console_directly()
    ///         .await
    /// }
    /// ```
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Register a slash command for the console REPL (spec 3.1 §8.1).
    ///
    /// `/help` lists registered commands. See [`ConsoleCommand`].
    pub fn with_console_command(mut self, command: Arc<dyn ConsoleCommand>) -> Self {
        self.console_commands.push(command);
        self
    }

    /// Serve the approval inbox for `service` under `/api/approvals` (spec 3.1 §8.2).
    ///
    /// Serve mode mounts the endpoints through
    /// [`ServerBuilder::with_approval_service`] and runs the approval ticker every 30
    /// seconds so requests expire and escalate. The endpoints answer `401` unless a
    /// [`with_request_context_extractor`](Self::with_request_context_extractor)
    /// authenticates the caller.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_core::{Agent, InMemoryApprovalStore, StoreApprovalService};
    ///
    /// async fn serve(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    ///     let approvals = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()));
    ///     Launcher::new(agent).with_approval_service(Arc::new(approvals)).run_serve_directly(8080).await
    /// }
    /// ```
    pub fn with_approval_service(mut self, service: Arc<dyn ApprovalService>) -> Self {
        self.approval_service = Some(service);
        self
    }

    /// Serve background runs from `state` under `/api/runs` (spec 3.1 §8.2).
    ///
    /// Serve mode mounts them through [`ServerBuilder::with_background_runs`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    ///
    /// use adk_cli::Launcher;
    /// use adk_core::Agent;
    /// use adk_server::background::BackgroundState;
    ///
    /// async fn serve(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    ///     Launcher::new(agent).with_background_runs(BackgroundState::new()).run_serve_directly(8080).await
    /// }
    /// ```
    pub fn with_background_runs(mut self, state: BackgroundState) -> Self {
        self.background_runs = Some(state);
        self
    }

    /// Mounts `routes` under `/api` behind the auth middleware in serve mode
    /// (spec 3.1 §8.2: the served harness's standing rules).
    #[cfg(feature = "harness")]
    pub(crate) fn with_api_routes(mut self, routes: Router) -> Self {
        self.api_routes.push(routes);
        self
    }

    /// Serves the web home over `engine`: the page at `/ui/` and its API under
    /// `/api/surface` (spec 3.3 §5.1, §10 S1).
    #[cfg(all(feature = "surface", feature = "harness"))]
    pub(crate) fn with_surface(mut self, engine: adk_surface::SurfaceEngine) -> Self {
        self.surface = Some(engine);
        self
    }

    /// Shares `control` with the server: its runs and background runs stop when it freezes
    /// (spec 3.3 §11 S-I10).
    #[cfg(feature = "harness")]
    pub(crate) fn with_governance(mut self, control: adk_core::GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Merges `routes` at the root in serve mode, behind the host and origin checks but not
    /// the auth middleware (spec 3.3 §10 K2: the device pairing claim needs no caller).
    #[cfg(feature = "harness")]
    pub(crate) fn with_root_routes(mut self, routes: Router) -> Self {
        self.root_routes.push(routes);
        self
    }

    /// Shares `control` with the server and serves the admin governance endpoints, whose
    /// freeze `delegate` handles (spec 3.2 §10 S4: a kernel parks running processes).
    #[cfg(feature = "harness")]
    pub(crate) fn with_kernel_governance(
        mut self,
        control: adk_core::GovernanceControl,
        delegate: Arc<dyn adk_server::rest::FreezeDelegate>,
    ) -> Self {
        self.kernel_governance = Some((control, delegate));
        self
    }

    /// Control how emitted thinking content is rendered in console mode.
    pub fn with_thinking_mode(mut self, mode: ThinkingDisplayMode) -> Self {
        self.thinking_mode = mode;
        self
    }

    /// Run the launcher, parsing CLI arguments.
    ///
    /// - No arguments or `chat`: Interactive console
    /// - `serve [--port PORT] [--host ADDRESS]`: Web server with UI, on `127.0.0.1` unless
    ///   `--host` or [`with_bind_address`](Self::with_bind_address) says otherwise
    pub async fn run(self) -> Result<()> {
        let cli = LauncherCli::parse();

        match cli.command.unwrap_or(LauncherCommand::Chat) {
            LauncherCommand::Chat => self.run_console_directly().await,
            LauncherCommand::Serve { port, host } => {
                let launcher = match host {
                    Some(host) => self.with_bind_address(host),
                    None => self,
                };
                launcher.run_serve_directly(port).await
            }
            #[cfg(feature = "optimize")]
            LauncherCommand::Optimize(args) => {
                tracing::info!(
                    agent_path = %args.agent_path.display(),
                    eval_set_path = %args.eval_set_path.display(),
                    max_iterations = args.max_iterations,
                    target = args.target,
                    output = %args.output.display(),
                    "starting prompt optimization"
                );
                // The actual optimization wiring is done by the caller
                // who has access to the optimizer LLM and evaluator.
                // This branch serves as the CLI entry point.
                tracing::warn!(
                    "adk optimize requires wiring via PromptOptimizer — use adk-eval directly"
                );
                Ok(())
            }
        }
    }

    /// Run in interactive console mode without parsing CLI arguments.
    ///
    /// Use this when you already know you want console mode (e.g. from
    /// your own CLI parser). [`run`](Self::run) calls this internally.
    pub async fn run_console_directly(self) -> Result<()> {
        let console = ConsoleIo::terminal()?;
        let run_config = self.console_run_config(&console);
        let app_name = self.app_name.unwrap_or_else(|| self.agent.name().to_string());
        let user_id = self.user_id.unwrap_or_else(|| "user".to_string());
        let thinking_mode = self.thinking_mode;
        let agent = self.agent;
        let artifact_service = self.artifact_service;
        let memory_service = self.memory_service;
        let commands = self.console_commands;

        let session_service =
            self.session_service.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
        let session_id =
            open_console_session(session_service.as_ref(), &app_name, &user_id, self.session_id)
                .await?;
        let command_context = ConsoleCommandContext::new(
            app_name.clone(),
            user_id.clone(),
            session_id.clone(),
            session_service.clone(),
            console.clone(),
        );

        print_banner(agent.name(), &session_id);
        console.set_command_completions(command_completions(&commands));

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
            if trimmed == "/help" {
                print!("{}", help_text(&commands));
                continue;
            }
            if trimmed == "/clear" {
                println!("(conversation cleared — session state is unchanged)");
                continue;
            }

            console.add_history(&line);

            if let Some((command, args)) = find_command(&commands, &trimmed) {
                match command.run(args, &command_context).await {
                    Ok(output) if output.is_empty() => {}
                    Ok(output) => println!("{}", terminal_safe(output.trim_end())),
                    Err(e) => eprintln!("Error: {}", terminal_safe(&e.to_string())),
                }
                continue;
            }

            let user_content = Content::new("user").with_text(trimmed);
            println!();

            let cancellation_token = CancellationToken::new();
            let mut runner_builder = Runner::builder()
                .app_name(app_name.clone())
                .agent(agent.clone())
                .session_service(session_service.clone())
                .cancellation_token(cancellation_token.clone())
                .run_config(run_config.clone());
            if let Some(ref artifact_service) = artifact_service {
                runner_builder = runner_builder.artifact_service(artifact_service.clone());
            }
            if let Some(ref memory_service) = memory_service {
                runner_builder = runner_builder.memory_service(memory_service.clone());
            }
            let runner = runner_builder.build()?;
            // Ctrl-C at an approval prompt cancels this run through the console.
            console.set_run(Some(cancellation_token.clone()));
            let started = runner
                .run(
                    UserId::new(user_id.clone())?,
                    SessionId::new(session_id.clone())?,
                    user_content,
                )
                .await;
            let events = match started {
                Ok(events) => events,
                Err(error) => {
                    console.set_run(None);
                    return Err(error);
                }
            };
            let turn = drive_turn(events, &console, &cancellation_token, thinking_mode, true).await;
            console.set_run(None);
            if turn.ended_by_denial {
                println!("\nTurn ended: you denied a call. Say what to do instead.\n");
                continue;
            }
            if turn.interrupted {
                println!("\nInterrupted.\n");
                continue;
            }

            println!("\n");
        }

        Ok(())
    }

    /// The run configuration of console runs: [`with_run_config`](Self::with_run_config)
    /// with the console settings applied.
    fn console_run_config(&self, console: &ConsoleIo) -> RunConfig {
        let mut config = self.run_config.clone().unwrap_or_default();
        if let Some(mode) = self.streaming_mode {
            config.streaming_mode = mode;
        }
        if let Some(policy) = &self.tool_policy {
            config.tool_policy = Some(policy.clone());
        }
        if let Some(handler) = &self.confirmation_handler {
            config.tool_confirmation_handler = Some(handler.clone());
        }
        if config.tool_confirmation_handler.is_none() {
            config.tool_confirmation_handler =
                Some(Arc::new(ConsoleConfirmationHandler::new(console.clone())));
        }
        // A person reads and answers the prompt; the library default suits services.
        if config.tool_confirmation_timeout == DEFAULT_TOOL_CONFIRMATION_TIMEOUT {
            config.tool_confirmation_timeout = CONSOLE_CONFIRMATION_TIMEOUT;
        }
        config
    }

    fn init_telemetry(&self) -> Option<Arc<adk_telemetry::AdkSpanExporter>> {
        match &self.telemetry_config {
            TelemetryConfig::AdkExporter { service_name } => {
                match adk_telemetry::init_with_adk_exporter(service_name) {
                    Ok(exporter) => Some(exporter),
                    Err(e) => {
                        warn!("failed to initialize telemetry: {e}");
                        None
                    }
                }
            }
            TelemetryConfig::Otlp { service_name, endpoint } => {
                #[cfg(feature = "telemetry-otlp")]
                if let Err(e) = adk_telemetry::init_with_otlp(service_name, endpoint) {
                    warn!("failed to initialize otlp telemetry: {e}");
                }
                #[cfg(not(feature = "telemetry-otlp"))]
                warn!(
                    service.name = %service_name,
                    otlp.endpoint = %endpoint,
                    "otlp telemetry requested but the telemetry-otlp feature is disabled"
                );
                None
            }
            TelemetryConfig::None => None,
        }
    }

    fn into_server_config(
        self,
        span_exporter: Option<Arc<adk_telemetry::AdkSpanExporter>>,
    ) -> ServerConfig {
        let session_service =
            self.session_service.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));
        let agent_loader = Arc::new(adk_core::SingleAgentLoader::new(self.agent));

        let mut config = ServerConfig::new(agent_loader, session_service)
            .with_artifact_service_opt(self.artifact_service);

        if let Some(memory_service) = self.memory_service {
            config = config.with_memory_service(memory_service);
        }

        if let Some(compaction_config) = self.compaction_config {
            config = config.with_compaction(compaction_config);
        }

        if let (Some(context_cache_config), Some(cache_capable)) =
            (self.context_cache_config, self.cache_capable)
        {
            config = config.with_context_cache(context_cache_config, cache_capable);
        }

        if let Some(security) = self.security_config {
            config = config.with_security(security);
        }

        if let Some(extractor) = self.request_context_extractor {
            config = config.with_request_context(extractor);
        }

        if let Some(exporter) = span_exporter {
            config = config.with_span_exporter(exporter);
        }

        config
    }

    /// Build the Axum application without serving it.
    ///
    /// This is the production escape hatch for adding custom routes,
    /// middleware, metrics, or owning the serve loop yourself. The router comes from
    /// [`ServerBuilder`], with A2A routes when [`with_a2a_base_url`](Self::with_a2a_base_url)
    /// is set, the approval inbox and its ticker when
    /// [`with_approval_service`](Self::with_approval_service) is set (the ticker starts only
    /// inside a Tokio runtime), and background runs when
    /// [`with_background_runs`](Self::with_background_runs) is set. The web home of
    /// `adk-surface` is served by `adk-rust serve` and `adk-rust up`, over the harness
    /// (spec 3.3 §5.1, §10 S1); a launcher serving a chat agent serves its REST API.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a tool policy is set through
    /// [`with_tool_policy`](Self::with_tool_policy) or [`with_run_config`](Self::with_run_config):
    /// the server runs without the launcher's `RunConfig`, so serving would drop the policy.
    pub fn build_app(self) -> Result<Router> {
        if self.tool_policy.is_some()
            || self.run_config.as_ref().is_some_and(|config| config.tool_policy.is_some())
        {
            return Err(AdkError::config(
                "the launcher's tool policy applies to console runs, and serve mode cannot \
                 apply it: the server does not take the launcher's RunConfig. Apply the \
                 policy in the agent through Agent::configure_run, or remove it before serving",
            ));
        }
        let span_exporter = self.init_telemetry();
        let a2a_base_url = self.a2a_base_url.clone();
        let approval_service = self.approval_service.clone();
        let background_runs = self.background_runs.clone();
        let api_routes = self.api_routes.clone();
        #[cfg(all(feature = "surface", feature = "harness"))]
        let surface = self.surface.clone();
        let root_routes = self.root_routes.clone();
        #[cfg(feature = "harness")]
        let kernel_governance = self.kernel_governance.clone();
        #[cfg(feature = "harness")]
        let governance = self.governance.clone();
        #[cfg_attr(not(feature = "harness"), allow(unused_mut))]
        let mut config = self.into_server_config(span_exporter);
        #[cfg(feature = "harness")]
        if let Some(control) =
            kernel_governance.as_ref().map(|(control, _)| control).or(governance.as_ref())
        {
            config = config.with_governance(control.clone());
        }

        let mut builder = ServerBuilder::new(config);
        // The web home (spec 3.3 §5.1, §10 S1): the page at `/ui/` and the engine's API under
        // `/api/surface`, behind the server's authentication (§11 S-I8).
        #[cfg(all(feature = "surface", feature = "harness"))]
        if let Some(surface) = surface {
            builder = builder
                .add_api_routes(adk_surface::web::router(surface))
                .add_root_routes(adk_surface::web::home());
        }
        #[cfg(feature = "harness")]
        if let Some((_, delegate)) = kernel_governance {
            builder = builder.with_freeze_delegate(delegate);
        }
        if let Some(base_url) = a2a_base_url {
            builder = builder.with_a2a(base_url);
        }
        if let Some(service) = approval_service {
            if tokio::runtime::Handle::try_current().is_ok() {
                adk_server::approvals::spawn_approval_ticker(
                    service.clone(),
                    APPROVAL_TICK_INTERVAL,
                );
            }
            builder = builder.with_approval_service(service);
        }
        if let Some(state) = background_runs {
            builder = builder.with_background_runs(state);
        }
        for routes in api_routes {
            builder = builder.add_api_routes(routes);
        }
        for routes in root_routes {
            builder = builder.add_root_routes(routes);
        }
        Ok(builder.build())
    }

    /// Build the Axum application with A2A routes enabled.
    pub fn build_app_with_a2a(mut self, base_url: impl Into<String>) -> Result<Router> {
        self.a2a_base_url = Some(base_url.into());
        self.build_app()
    }

    /// Run web server without parsing CLI arguments.
    ///
    /// Listens on the [bind address](Self::with_bind_address), `127.0.0.1` by default.
    /// Use this when you already know you want serve mode (e.g. from
    /// your own CLI parser). [`run`](Self::run) calls this internally.
    pub async fn run_serve_directly(self, port: u16) -> Result<()> {
        let addr = SocketAddr::new(self.bind_address, port);
        let app = self.build_app()?;

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| adk_core::AdkError::config(format!("failed to bind {addr}: {e}")))?;

        if !addr.ip().is_loopback() {
            warn!(
                server.address = %addr,
                "serving beyond loopback; configure authentication, and list public host names in SecurityConfig::allowed_hosts"
            );
        }
        print!("{}", serve_banner(addr));

        // The peer address lets an extractor such as the harness's loopback operator see who
        // connected.
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| adk_core::AdkError::config(format!("server error: {e}")))?;

        Ok(())
    }
}

/// What serve mode prints on start: where the REST API answers. The server mounts nothing at
/// `/`; the web home is `adk-rust serve`'s and `adk-rust up`'s (spec 3.3 §10 S1).
fn serve_banner(addr: SocketAddr) -> String {
    format!(
        "ADK Server starting on http://{addr}\n  REST API: http://{addr}/api (health: /api/health)\nPress Ctrl+C to stop\n\n"
    )
}

/// Opens the console's session: `requested` when it exists, otherwise a new session,
/// named `requested` when one was asked for.
async fn open_console_session(
    sessions: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    requested: Option<String>,
) -> Result<String> {
    if let Some(session_id) = &requested {
        let existing = sessions
            .get(GetRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.clone(),
                num_recent_events: Some(1),
                after: None,
            })
            .await;
        match existing {
            Ok(session) => return Ok(session.id().to_string()),
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }
    }
    let created = sessions
        .create(CreateRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: requested,
            state: HashMap::new(),
        })
        .await?;
    Ok(created.id().to_string())
}

/// How one console turn ended.
#[derive(Debug, Default)]
pub(crate) struct TurnEnd {
    /// The run was cancelled: Ctrl-C during the run or at an approval prompt.
    pub(crate) interrupted: bool,
    /// The run stopped after a person denied a call, once the denial was recorded
    /// (spec 3.1 §9.2 I19).
    pub(crate) ended_by_denial: bool,
    /// Calls the run held for a decision instead of deciding them at the console.
    pub(crate) held: Vec<ToolConfirmationRequest>,
    /// The text the agents wrote, without thoughts.
    pub(crate) text: String,
}

/// Prints one run's events until it ends, Ctrl-C cancels it, or a person's denial ends
/// the turn (spec 3.1 §8.1).
///
/// The runner records each event before yielding it, so stopping right after the event
/// that carries the denied call's response leaves a complete call and response in the
/// session and makes no further model call.
///
/// With `echo` false nothing is printed; the text is still collected.
pub(crate) async fn drive_turn(
    mut events: adk_core::EventStream,
    console: &ConsoleIo,
    cancellation_token: &CancellationToken,
    thinking_mode: ThinkingDisplayMode,
    echo: bool,
) -> TurnEnd {
    let mut printer = StreamPrinter::new(thinking_mode);
    let mut text_deltas = adk_core::EventTextDeltas::default();
    let mut current_agent = String::new();
    let mut printed_header = false;
    let mut end = TurnEnd::default();

    loop {
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else {
                    break;
                };

                match event {
                    Ok(evt) => {
                        let evt = text_deltas.push(&evt);
                        if evt.author != "user"
                            && let Some(content) = &evt.llm_response.content
                        {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    end.text.push_str(text);
                                }
                            }
                        }
                        // Track agent switches in multi-agent workflows
                        if echo
                            && !evt.author.is_empty()
                            && evt.author != "user"
                            && evt.author != current_agent
                        {
                            if !current_agent.is_empty() {
                                println!();
                            }
                            current_agent = evt.author.clone();
                            // Only show agent label in multi-agent scenarios
                            if printed_header {
                                print!(
                                    "\x1b[33m[{}]\x1b[0m ",
                                    terminal_safe(&current_agent)
                                );
                                let _ = io::stdout().flush();
                            }
                            printed_header = true;
                        }

                        // Show agent transfer requests
                        if echo && let Some(target) = &evt.actions.transfer_to_agent {
                            print!(
                                "\x1b[90m[transfer -> {}]\x1b[0m ",
                                terminal_safe(target)
                            );
                            let _ = io::stdout().flush();
                        }

                        // A decision a prompter printed for a call follows the call's line
                        // and precedes its response (spec 3.1 §8.1).
                        let (calls, responses) = call_ids(&evt);
                        for id in &responses {
                            console.call_shown(id);
                        }
                        if echo {
                            printer.handle_event(&evt);
                        }
                        for id in &calls {
                            console.call_shown(id);
                        }
                        if let Some(request) = &evt.actions.tool_confirmation {
                            end.held.push(request.clone());
                        }
                        let answered_calls = !evt.llm_response.partial
                            && evt.llm_response.content.as_ref().is_some_and(|content| {
                                content
                                    .parts
                                    .iter()
                                    .any(|part| matches!(part, Part::FunctionResponse { .. }))
                            });
                        if answered_calls && console.end_turn_requested() {
                            cancellation_token.cancel();
                            end.ended_by_denial = true;
                            break;
                        }
                    }
                    Err(e) => {
                        // The message is shown here, escaped; the log line
                        // stays content-free, since it can be exported.
                        eprintln!("\nError: {}", terminal_safe(&e.to_string()));
                        error!(error = %e.diagnostics(), "stream error");
                    }
                }
            }
            () = cancellation_token.cancelled() => {
                end.interrupted = true;
                break;
            }
            signal = tokio::signal::ctrl_c() => {
                match signal {
                    Ok(()) => {
                        cancellation_token.cancel();
                        end.interrupted = true;
                        break;
                    }
                    Err(err) => {
                        error!("failed to listen for Ctrl+C: {err}");
                    }
                }
            }
        }
    }

    if echo {
        printer.finish();
    }
    console.flush_call_prints();
    // The stream can end on its own once a prompt cancelled the run.
    if cancellation_token.is_cancelled() && !end.ended_by_denial {
        end.interrupted = true;
    }
    end
}

/// The ids of the function calls and of the function responses a complete event shows.
fn call_ids(event: &Event) -> (Vec<String>, Vec<String>) {
    let (mut calls, mut responses) = (Vec::new(), Vec::new());
    if event.llm_response.partial {
        return (calls, responses);
    }
    for part in event.llm_response.content.as_ref().map_or(&[][..], |c| c.parts.as_slice()) {
        match part {
            Part::FunctionCall { id: Some(id), .. } => calls.push(id.clone()),
            Part::FunctionResponse { id: Some(id), .. } => responses.push(id.clone()),
            _ => {}
        }
    }
    (calls, responses)
}

/// The slash commands Tab completes at the start of a line: the built-ins and `commands`.
pub(crate) fn command_completions(commands: &[Arc<dyn ConsoleCommand>]) -> Vec<String> {
    let mut names: Vec<String> =
        ["/help", "/clear", "/quit", "/exit"].iter().map(ToString::to_string).collect();
    names.extend(
        commands.iter().map(|command| format!("/{}", command.name().trim_start_matches('/'))),
    );
    names.sort_unstable();
    names.dedup();
    names
}

/// The registered command a slash line names, with the text after the name.
pub(crate) fn find_command<'a>(
    commands: &'a [Arc<dyn ConsoleCommand>],
    line: &'a str,
) -> Option<(&'a Arc<dyn ConsoleCommand>, &'a str)> {
    let invocation = line.strip_prefix('/')?;
    let (name, args) = invocation.split_once(char::is_whitespace).unwrap_or((invocation, ""));
    commands
        .iter()
        .find(|command| command.name().trim_start_matches('/') == name)
        .map(|command| (command, args.trim()))
}

/// Print the ADK-Rust welcome banner.
fn print_banner(agent_name: &str, session_id: &str) {
    let version = env!("CARGO_PKG_VERSION");
    let title = format!("ADK-Rust  v{version}");
    let subtitle = "Rust Agent Development Kit";
    // Box inner width = 49 (between the ║ chars)
    let inner: usize = 49;
    let pad_title = (inner.saturating_sub(title.len())) / 2;
    let pad_subtitle = (inner.saturating_sub(subtitle.len())) / 2;

    println!();
    println!("    ╔{:═<inner$}╗", "");
    println!(
        "    ║{:>w$}{title}{:<r$}║",
        "",
        "",
        w = pad_title,
        r = inner - pad_title - title.len()
    );
    println!(
        "    ║{:>w$}{subtitle}{:<r$}║",
        "",
        "",
        w = pad_subtitle,
        r = inner - pad_subtitle - subtitle.len()
    );
    println!("    ╚{:═<inner$}╝", "");
    println!();
    println!("  Agent    : {agent_name}");
    println!("  Session  : {}", terminal_safe(session_id));
    println!("  Runtime  : Tokio async, streaming responses");
    println!("  Features : tool calling, multi-provider, multi-agent, think blocks");
    println!();
    println!("  Type a message to chat. /help for commands.");
    println!();
}

/// The REPL commands, built-in and registered, for `/help`.
pub(crate) fn help_text(commands: &[Arc<dyn ConsoleCommand>]) -> String {
    let mut rows = vec![
        ("/help".to_string(), "Show this help".to_string()),
        ("/clear".to_string(), "Clear display (session state is kept)".to_string()),
    ];
    for command in commands {
        let name = command.name().trim_start_matches('/');
        if !matches!(name, "help" | "clear" | "quit" | "exit") {
            rows.push((format!("/{name}"), command.description().to_string()));
        }
    }
    for exit in ["quit", "exit", "/quit", "/exit"] {
        rows.push((exit.to_string(), "Exit the REPL".to_string()));
    }
    let width = rows.iter().map(|(name, _)| name.chars().count()).max().unwrap_or(0);

    let mut text = String::from("\n  Commands:\n");
    for (name, description) in rows {
        text.push_str(&format!("    {name:<width$}  {description}\n"));
    }
    text.push_str(
        "\n  Tips:\n    - Up/Down arrows browse history\n    - Ctrl+C interrupts the current \
         operation, and denies the call at an approval prompt\n    - Multi-agent workflows \
         show [agent_name] on handoff\n\n",
    );
    terminal_safe(&text).into_owned()
}

pub(crate) fn is_exit_command(input: &str) -> bool {
    matches!(input, "quit" | "exit" | "/quit" | "/exit")
}

/// The text of a provider message item carried as a server tool call (`type: "message"`
/// with `output_text` content), if `item` is one.
fn message_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let text: String = item
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    Some(text)
}

/// Escapes terminal control characters in model- or tool-controlled text.
///
/// Newlines and tabs are kept. Every other control character, such as the `ESC` that starts
/// an ANSI or OSC sequence or a C1 `CSI`, is written as its `\u{..}` escape, so printed text
/// cannot move the cursor, rewrite the screen or window title, or set the clipboard.
pub(crate) fn terminal_safe(text: &str) -> std::borrow::Cow<'_, str> {
    let unsafe_char = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !text.chars().any(unsafe_char) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if unsafe_char(c) {
            escaped.extend(c.escape_unicode());
        } else {
            escaped.push(c);
        }
    }
    std::borrow::Cow::Owned(escaped)
}

/// Handles streaming output with special rendering for think blocks,
/// tool calls, function responses, and inline/file data.
///
/// Model and tool text is printed with terminal control characters escaped, apart from
/// newlines and tabs, so a response cannot inject escape sequences into the terminal.
///
/// A call held for confirmation is shown as `[approval needed] <tool> <args> — <reason>`,
/// and the decision applied to a call as `[approved] <tool>` or `[denied] <tool>`
/// (spec 3.1 §8.1).
pub struct StreamPrinter<W: Write = io::Stdout> {
    thinking_mode: ThinkingDisplayMode,
    in_think_block: bool,
    in_thinking_part_stream: bool,
    think_buffer: String,
    /// Why each held call is waiting, by function call id (tool name for calls without
    /// one), from its pending response.
    hold_reasons: HashMap<String, String>,
    out: W,
}

impl StreamPrinter {
    /// Create a printer that writes to stdout with the selected thinking display mode.
    pub fn new(thinking_mode: ThinkingDisplayMode) -> Self {
        Self::with_writer(thinking_mode, io::stdout())
    }
}

impl<W: Write> StreamPrinter<W> {
    /// Create a printer that writes to `out` instead of stdout.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::launcher::{StreamPrinter, ThinkingDisplayMode};
    /// use adk_core::Part;
    ///
    /// let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, std::io::sink());
    /// printer.handle_part(&Part::Text { text: "hello".into() });
    /// printer.finish();
    /// ```
    pub fn with_writer(thinking_mode: ThinkingDisplayMode, out: W) -> Self {
        Self {
            thinking_mode,
            in_think_block: false,
            in_thinking_part_stream: false,
            think_buffer: String::new(),
            hold_reasons: HashMap::new(),
            out,
        }
    }

    /// Writes `text` with terminal control characters escaped, then flushes.
    fn emit(&mut self, text: &str) {
        let _ = self.out.write_all(terminal_safe(text).as_bytes());
        let _ = self.out.flush();
    }

    /// Render every part of `event`.
    ///
    /// A streamed function call arrives in a partial chunk and again in the complete event,
    /// so a function call is rendered from the complete event only. An event that holds a
    /// call for confirmation ([`EventActions::tool_confirmation`](adk_core::EventActions))
    /// is rendered as `[approval needed] <tool> <args> — <reason>` in place of its text,
    /// and an applied decision
    /// ([`EventActions::tool_confirmation_decision`](adk_core::EventActions)) as
    /// `[approved] <tool>` or `[denied] <tool>` ahead of the tool's response.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::launcher::{StreamPrinter, ThinkingDisplayMode};
    /// use adk_core::{Content, Event, Part};
    ///
    /// let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, Vec::new());
    /// let mut event = Event::new("invocation");
    /// event.set_content(Content::new("model").with_text("hello"));
    /// printer.handle_event(&event);
    /// printer.finish();
    /// ```
    pub fn handle_event(&mut self, event: &Event) {
        // A note from an `on_hold` or `on_stop` hook is for the user (spec 3.1 §7.22).
        #[cfg(feature = "harness")]
        if let Some(notice) = event.provider_metadata.get(adk_harness::hooks::HOOK_NOTICE_KEY) {
            self.flush_pending_thinking();
            let (source, text) = hook_notice(notice);
            let text = text.replace('\n', "\n  ");
            self.emit(&format!("\n[hook:{source}] {text}\n"));
        }
        let held = event.actions.tool_confirmation.as_ref();
        let decision = event.actions.tool_confirmation_decision;
        let parts = event.llm_response.content.as_ref().map_or(&[][..], |c| c.parts.as_slice());
        for part in parts {
            match part {
                Part::FunctionCall { .. } if event.llm_response.partial => continue,
                // Provider items replayed to the model (such as OpenAI Responses commentary)
                // stream as partial chunks; the complete item is shown once.
                Part::ServerToolCall { .. } if event.llm_response.partial => continue,
                // The held call's line below says what the text says, with the call.
                Part::Text { .. } if held.is_some() => continue,
                Part::FunctionResponse { function_response, id, .. } => {
                    if let Some(decision) = decision {
                        self.flush_pending_thinking();
                        let label = match decision {
                            ToolConfirmationDecision::Approve => "approved",
                            ToolConfirmationDecision::Deny => "denied",
                        };
                        self.emit(&format!("\n[{label}] {}\n", function_response.name));
                    }
                    if adk_core::is_confirmation_pending(&function_response.response)
                        && let Some(reason) =
                            function_response.response.get("error").and_then(Value::as_str)
                    {
                        let key = id.as_ref().unwrap_or(&function_response.name);
                        self.hold_reasons.insert(key.clone(), reason.to_string());
                    }
                }
                _ => {}
            }
            self.handle_part(part);
        }
        if let Some(request) = held {
            self.print_held_call(request);
        }
    }

    fn print_held_call(&mut self, request: &ToolConfirmationRequest) {
        self.flush_pending_thinking();
        let reason = request
            .function_call_id
            .as_ref()
            .and_then(|id| self.hold_reasons.remove(id))
            .or_else(|| self.hold_reasons.remove(&request.tool_name));
        let line = match reason {
            Some(reason) => {
                format!("\n[approval needed] {} {} — {reason}\n", request.tool_name, request.args)
            }
            None => format!("\n[approval needed] {} {}\n", request.tool_name, request.args),
        };
        self.emit(&line);
    }

    /// Process a single response part, rendering it to the printer's writer.
    pub fn handle_part(&mut self, part: &Part) {
        match part {
            Part::Text { text } => {
                self.flush_part_thinking_if_needed();
                self.handle_text_chunk(text);
            }
            Part::Thinking { thinking, .. } => {
                // A snapshot reduced to its unseen suffix can leave an empty thinking part.
                if thinking.is_empty() || matches!(self.thinking_mode, ThinkingDisplayMode::Hide) {
                    return;
                }
                if !self.in_thinking_part_stream {
                    self.emit("\n[thinking] ");
                    self.in_thinking_part_stream = true;
                }
                self.think_buffer.push_str(thinking);
                self.emit(thinking);
            }
            Part::FunctionCall { name, args, .. } => {
                self.flush_pending_thinking();
                self.emit(&format!("\n[tool-call] {name} {args}\n"));
            }
            Part::FunctionResponse { function_response, .. } => {
                self.flush_pending_thinking();
                self.print_tool_response(&function_response.name, &function_response.response);
            }
            Part::InlineData { mime_type, data, .. } => {
                self.flush_pending_thinking();
                self.emit(&format!("\n[inline-data] mime={mime_type} bytes={}\n", data.len()));
            }
            Part::FileData { mime_type, file_uri, .. } => {
                self.flush_pending_thinking();
                self.emit(&format!("\n[file-data] mime={mime_type} uri={file_uri}\n"));
            }
            Part::ServerToolCall { server_tool_call } => {
                self.flush_pending_thinking();
                match message_text(server_tool_call) {
                    // A model message kept for replay, such as OpenAI Responses commentary:
                    // its text is what the model said.
                    Some(text) => self.emit(&format!("{text}\n")),
                    None => self.emit(&format!("\n[server-tool-call] {server_tool_call}\n")),
                }
            }
            Part::ServerToolResponse { server_tool_response } => {
                self.flush_pending_thinking();
                self.emit(&format!(
                    "\n[server-tool-response] {}B\n",
                    server_tool_response.to_string().len()
                ));
            }
            Part::EmbeddedResource { resource } => {
                self.flush_pending_thinking();
                let line = match resource {
                    adk_core::EmbeddedResource::Text(text) => format!(
                        "\n[embedded-resource] uri={} text-len={}\n",
                        text.uri,
                        text.text.len()
                    ),
                    adk_core::EmbeddedResource::Blob(blob) => format!(
                        "\n[embedded-resource] uri={} bytes={}\n",
                        blob.uri,
                        blob.data.len()
                    ),
                };
                self.emit(&line);
            }
        }
    }

    fn handle_text_chunk(&mut self, chunk: &str) {
        if matches!(self.thinking_mode, ThinkingDisplayMode::Hide) {
            let mut visible = String::with_capacity(chunk.len());
            let mut remaining = chunk;

            while let Some(start_idx) = remaining.find("<think>") {
                visible.push_str(&remaining[..start_idx]);
                let after_start = &remaining[start_idx + "<think>".len()..];
                if let Some(end_idx) = after_start.find("</think>") {
                    remaining = &after_start[end_idx + "</think>".len()..];
                } else {
                    remaining = "";
                    break;
                }
            }

            visible.push_str(remaining);
            if !visible.is_empty() {
                self.emit(&visible);
            }
            return;
        }

        const THINK_START: &str = "<think>";
        const THINK_END: &str = "</think>";

        let mut remaining = chunk;

        while !remaining.is_empty() {
            if self.in_think_block {
                if let Some(end_idx) = remaining.find(THINK_END) {
                    self.think_buffer.push_str(&remaining[..end_idx]);
                    self.flush_think();
                    self.in_think_block = false;
                    remaining = &remaining[end_idx + THINK_END.len()..];
                } else {
                    self.think_buffer.push_str(remaining);
                    break;
                }
            } else if let Some(start_idx) = remaining.find(THINK_START) {
                let visible = &remaining[..start_idx];
                if !visible.is_empty() {
                    self.emit(visible);
                }
                self.in_think_block = true;
                self.think_buffer.clear();
                remaining = &remaining[start_idx + THINK_START.len()..];
            } else {
                self.emit(remaining);
                break;
            }
        }
    }

    fn flush_think(&mut self) {
        let content = self.think_buffer.trim();
        if !content.is_empty() {
            let line = format!("\n[think] {content}\n");
            self.emit(&line);
        }
        self.think_buffer.clear();
    }

    /// Flush any pending think block content.
    pub fn finish(&mut self) {
        self.flush_pending_thinking();
    }

    fn print_tool_response(&mut self, name: &str, response: &Value) {
        self.emit(&format!("\n[tool-response] {name} {response}\n"));
    }

    fn flush_part_thinking_if_needed(&mut self) {
        if self.in_thinking_part_stream {
            self.emit("\n");
            self.think_buffer.clear();
            self.in_thinking_part_stream = false;
        }
    }

    fn flush_pending_thinking(&mut self) {
        self.flush_part_thinking_if_needed();
        if self.in_think_block {
            self.flush_think_with_label("think");
            self.in_think_block = false;
        }
    }

    fn flush_think_with_label(&mut self, label: &str) {
        let content = self.think_buffer.trim();
        if !content.is_empty() {
            let line = format!("\n[{label}] {content}\n");
            self.emit(&line);
        }
        self.think_buffer.clear();
    }
}

impl Default for StreamPrinter {
    fn default() -> Self {
        Self::new(ThinkingDisplayMode::Auto)
    }
}

/// The source and text of a rendered hook note (`<hook-note source="…" hook="…">` around
/// the text); a note in another shape is shown as it is, from `hook`.
#[cfg(feature = "harness")]
fn hook_notice(rendered: &str) -> (String, String) {
    let parsed = rendered
        .strip_prefix("<hook-note source=\"")
        .and_then(|rest| rest.split_once('"'))
        .and_then(|(source, rest)| {
            let body = rest.split_once(">\n")?.1;
            let text = body.strip_suffix("\n</hook-note>").unwrap_or(body);
            Some((source.to_string(), text.to_string()))
        });
    parsed.unwrap_or_else(|| ("hook".to_string(), rendered.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult};
    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use futures::stream;
    use std::sync::Arc;
    use tower::ServiceExt;

    #[cfg(feature = "harness")]
    #[test]
    fn a_hook_notice_is_shown_to_the_user_with_its_source() {
        use adk_harness::hooks::{HOOK_NOTICE_KEY, HookNote, HookSource};
        let note = HookNote::new(HookSource::Workspace, "sha256:ab12", "turn done\ntwo files");
        let mut event = Event::new("invocation");
        event.provider_metadata.insert(HOOK_NOTICE_KEY.to_string(), note.render());
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        printer.handle_event(&event);
        printer.finish();
        assert_eq!(captured.text(), "\n[hook:workspace] turn done\n  two files\n");
        assert_eq!(
            hook_notice("plain text"),
            ("hook".to_string(), "plain text".to_string()),
            "a note in another shape is shown as it is"
        );
    }

    #[test]
    fn command_completions_cover_the_built_ins_and_the_registered_commands() {
        struct Named(&'static str);
        #[async_trait]
        impl ConsoleCommand for Named {
            fn name(&self) -> &str {
                self.0
            }
            fn description(&self) -> &str {
                ""
            }
            async fn run(&self, _: &str, _: &ConsoleCommandContext) -> AdkResult<String> {
                Ok(String::new())
            }
        }
        let commands: Vec<Arc<dyn ConsoleCommand>> =
            vec![Arc::new(Named("mode")), Arc::new(Named("/mcp")), Arc::new(Named("help"))];
        assert_eq!(
            command_completions(&commands),
            ["/clear", "/exit", "/help", "/mcp", "/mode", "/quit"]
        );
    }

    struct TestAgent;

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            "launcher_test_agent"
        }

        fn description(&self) -> &str {
            "launcher test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            Ok(Box::pin(stream::empty()))
        }
    }

    fn test_launcher() -> Launcher {
        Launcher::new(Arc::new(TestAgent)).with_telemetry(TelemetryConfig::None)
    }

    #[test]
    fn serve_listens_on_loopback_unless_told_otherwise() {
        assert_eq!(test_launcher().bind_address, IpAddr::V4(Ipv4Addr::LOCALHOST));
        let exposed = test_launcher().with_bind_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(exposed.bind_address, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn serve_host_flag_is_an_explicit_opt_in() {
        let default = LauncherCli::try_parse_from(["agent", "serve"]).unwrap();
        assert!(matches!(default.command, Some(LauncherCommand::Serve { port: 8080, host: None })));

        let exposed = LauncherCli::try_parse_from(["agent", "serve", "--host", "0.0.0.0"]).unwrap();
        assert!(matches!(
            exposed.command,
            Some(LauncherCommand::Serve { host: Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)), .. })
        ));
    }

    #[tokio::test]
    async fn built_app_refuses_a_rebinding_host() {
        let app = test_launcher().build_app().unwrap();
        let request = |host: &str| {
            Request::builder().uri("/api/health").header("host", host).body(Body::empty()).unwrap()
        };

        let refused = app.clone().oneshot(request("rebind.attacker.example:8080")).await.unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);

        let answered = app.oneshot(request("localhost:8080")).await.unwrap();
        assert_eq!(answered.status(), StatusCode::OK);
    }

    #[test]
    fn stream_printer_tracks_think_block_state() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);
        assert!(!printer.in_think_block);

        // Opening a think block sets the flag
        printer.handle_text_chunk("<think>reasoning");
        assert!(printer.in_think_block);
        assert_eq!(printer.think_buffer, "reasoning");

        // Closing the think block clears the flag and buffer
        printer.handle_text_chunk(" more</think>visible");
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_handles_think_block_across_chunks() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_text_chunk("before<think>start");
        assert!(printer.in_think_block);
        assert_eq!(printer.think_buffer, "start");

        printer.handle_text_chunk(" middle");
        assert!(printer.in_think_block);
        assert_eq!(printer.think_buffer, "start middle");

        printer.handle_text_chunk(" end</think>after");
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_finish_flushes_open_think_block() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_text_chunk("<think>unclosed reasoning");
        assert!(printer.in_think_block);

        printer.finish();
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_finish_is_noop_when_no_think_block() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);
        printer.finish();
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_handles_multiple_think_blocks_in_one_chunk() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_text_chunk("a<think>first</think>b<think>second</think>c");
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_handles_empty_think_block() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_text_chunk("<think></think>after");
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn a_streamed_tool_call_is_printed_once() {
        let call = Part::FunctionCall {
            name: "get_weather".into(),
            args: serde_json::json!({"city": "Paris"}),
            id: Some("call-1".into()),
            thought_signature: None,
        };
        let mut chunk = Event::new("invocation");
        chunk.llm_response.partial = true;
        chunk.set_content(Content { role: "model".into(), parts: vec![call.clone()] });
        let mut complete = chunk.clone();
        complete.llm_response.partial = false;

        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, Vec::new());
        printer.handle_event(&chunk);
        printer.handle_event(&complete);
        printer.finish();

        let printed = String::from_utf8(printer.out).unwrap();
        assert_eq!(printed.matches("[tool-call] get_weather").count(), 1, "{printed}");
    }

    #[test]
    fn stream_printer_handles_all_part_types() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        // Text
        printer.handle_part(&Part::Text { text: "hello".into() });
        assert!(!printer.in_think_block);

        // Thinking
        printer.handle_part(&Part::Thinking { thinking: "reasoning".into(), signature: None });
        assert!(printer.in_thinking_part_stream);

        // FunctionCall
        printer.handle_part(&Part::FunctionCall {
            name: "get_weather".into(),
            args: serde_json::json!({"city": "Seattle"}),
            id: None,
            thought_signature: None,
        });

        // FunctionResponse
        printer.handle_part(&Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "get_weather",
                serde_json::json!({"temp": 72}),
            ),
            id: None,
            annotations: None,
        });

        // InlineData
        printer.handle_part(&Part::inline_data("image/png", vec![0u8; 100]));

        // FileData
        printer.handle_part(&Part::FileData {
            mime_type: "audio/wav".into(),
            file_uri: "gs://bucket/file.wav".into(),
            annotations: None,
        });

        // No panics, no state corruption
        assert!(!printer.in_think_block);
        assert!(!printer.in_thinking_part_stream);
    }

    #[test]
    fn stream_printer_text_without_think_tags_leaves_state_clean() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);
        printer.handle_text_chunk("just plain text with no tags");
        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_ignores_an_empty_thinking_part() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);
        printer.handle_part(&Part::Thinking { thinking: String::new(), signature: None });
        assert!(!printer.in_thinking_part_stream);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_coalesces_streamed_thinking_parts() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_part(&Part::Thinking { thinking: "Okay".into(), signature: None });
        printer.handle_part(&Part::Thinking { thinking: ", the".into(), signature: None });
        printer.handle_part(&Part::Thinking { thinking: " user".into(), signature: None });

        assert!(printer.in_thinking_part_stream);
        assert_eq!(printer.think_buffer, "Okay, the user");

        printer.handle_part(&Part::Text { text: "hello".into() });

        assert!(!printer.in_thinking_part_stream);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_finish_closes_streamed_thinking_state() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Auto);

        printer.handle_part(&Part::Thinking { thinking: "reasoning".into(), signature: None });
        assert!(printer.in_thinking_part_stream);

        printer.finish();

        assert!(!printer.in_thinking_part_stream);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_hide_mode_ignores_emitted_thinking_state() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Hide);

        printer.handle_part(&Part::Thinking { thinking: "secret".into(), signature: None });

        assert!(!printer.in_thinking_part_stream);
        assert!(printer.think_buffer.is_empty());
    }

    #[test]
    fn stream_printer_hide_mode_drops_think_tags_from_text() {
        let mut printer = StreamPrinter::new(ThinkingDisplayMode::Hide);

        printer.handle_text_chunk("visible<think>hidden</think>after");

        assert!(!printer.in_think_block);
        assert!(printer.think_buffer.is_empty());
    }

    /// A writer whose bytes the test can read after the printer is done.
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// An OSC 52 sequence, which sets the terminal clipboard, then a cursor move.
    const PLANTED: &str = "\x1b]52;c;cm0gLXJmIH4=\x07\x1b[2J\u{9b}31m";

    #[test]
    fn terminal_safe_escapes_control_characters_but_keeps_newlines_and_tabs() {
        assert_eq!(terminal_safe(PLANTED), "\\u{1b}]52;c;cm0gLXJmIH4=\\u{7}\\u{1b}[2J\\u{9b}31m");
        assert_eq!(terminal_safe("line\n\tindented\r"), "line\n\tindented\\u{d}");
        assert!(matches!(terminal_safe("plain text"), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn stream_printer_escapes_planted_sequences_in_every_line() {
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());

        printer.handle_part(&Part::Text { text: format!("answer {PLANTED}") });
        printer.handle_part(&Part::Text { text: format!("<think>plan {PLANTED}</think>") });
        printer.handle_part(&Part::Thinking {
            thinking: format!("reasoning {PLANTED}"),
            signature: None,
        });
        printer.handle_part(&Part::FunctionCall {
            name: format!("tool{PLANTED}"),
            args: serde_json::json!({"q": PLANTED}),
            id: None,
            thought_signature: None,
        });
        printer.handle_part(&Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                format!("tool{PLANTED}"),
                serde_json::json!({"r": PLANTED}),
            ),
            id: None,
            annotations: None,
        });
        printer.handle_part(&Part::FileData {
            mime_type: format!("image/png{PLANTED}"),
            file_uri: format!("https://example.com/{PLANTED}"),
            annotations: None,
        });
        printer.handle_part(&Part::EmbeddedResource {
            resource: adk_core::EmbeddedResource::Text(adk_core::TextResourceContents::new(
                format!("file:///{PLANTED}"),
                None,
                "body",
            )),
        });
        printer.handle_part(&Part::Text { text: format!("<think>unclosed {PLANTED}") });
        printer.finish();

        let output = captured.text();
        assert!(
            !output.chars().any(|c| c.is_control() && c != '\n' && c != '\t'),
            "raw control characters reached the terminal: {output:?}"
        );
        for line in [
            "answer \\u{1b}]52",
            "[think] plan \\u{1b}]52",
            "[thinking] reasoning \\u{1b}]52",
            "[tool-call] tool\\u{1b}]52",
            "[tool-response] tool\\u{1b}]52",
            "uri=https://example.com/\\u{1b}]52",
            "[embedded-resource] uri=file:///\\u{1b}]52",
            "[think] unclosed \\u{1b}]52",
        ] {
            assert!(output.contains(line), "missing {line:?} in {output:?}");
        }
    }

    #[test]
    fn exit_command_helper_accepts_plain_and_slash_variants() {
        for command in ["quit", "exit", "/quit", "/exit"] {
            assert!(is_exit_command(command));
        }

        assert!(!is_exit_command("hello"));
    }

    /// Every caller as `ana`.
    struct Ana;

    #[async_trait]
    impl adk_server::RequestContextExtractor for Ana {
        async fn extract(
            &self,
            _parts: &axum::http::request::Parts,
        ) -> std::result::Result<adk_core::RequestContext, adk_server::RequestContextError>
        {
            Ok(adk_core::RequestContext {
                user_id: "ana".into(),
                scopes: Vec::new(),
                metadata: Default::default(),
            })
        }
    }

    /// A served chat agent has no web home (spec 3.3 §10 S1): its Stop all would freeze every
    /// user's runs, and its runner has no approval inbox to show (`adk-rust serve` and `up`
    /// serve the home, over the harness).
    #[tokio::test]
    async fn a_served_chat_agent_has_no_web_home_or_its_api() {
        let app = test_launcher()
            .with_request_context_extractor(Arc::new(Ana))
            .build_app()
            .expect("launcher app should build");
        for uri in ["/ui/", "/api/surface/status?surface=web"] {
            let response =
                app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await;
            assert_eq!(response.unwrap().status(), StatusCode::NOT_FOUND, "{uri}");
        }
        let stop = Request::builder()
            .method("POST")
            .uri("/api/surface/commands")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"surface":"web","origin":{"kind":"pointer"},"command":{"type":"stopAll"}}"#,
            ))
            .unwrap();
        let response = app.oneshot(stop).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "no route freezes the server");
    }

    /// Serve mode names where the REST API answers; nothing is mounted at `/` to open.
    #[test]
    fn the_serve_banner_names_the_api_not_a_page_at_the_root() {
        let banner = serve_banner(SocketAddr::from(([127, 0, 0, 1], 8080)));
        assert!(banner.contains("http://127.0.0.1:8080/api"), "{banner}");
        assert!(!banner.contains("in your browser"), "{banner}");
    }

    #[tokio::test]
    async fn build_app_includes_health_route() {
        let app = test_launcher().build_app().expect("launcher app should build");

        let response = app
            .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
            .await
            .expect("health request should succeed");

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "healthy");
    }

    #[tokio::test]
    async fn build_app_does_not_enable_a2a_routes_by_default() {
        let app = test_launcher().build_app().expect("launcher app should build");

        let response = app
            .oneshot(Request::builder().uri("/.well-known/agent.json").body(Body::empty()).unwrap())
            .await
            .expect("agent card request should complete");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    fn held_call_events() -> (Event, Event) {
        let mut pending = Event::new("invocation");
        pending.set_content(Content {
            role: "user".into(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "record_note",
                    adk_core::confirmation_pending_response(
                        "Tool 'record_note' requires confirmation: notes are permanent",
                    ),
                ),
                id: Some("call-1".into()),
                annotations: None,
            }],
        });
        let mut held = Event::new("invocation");
        held.set_content(
            Content::new("model").with_text("Tool confirmation required for 'record_note'."),
        );
        held.actions.tool_confirmation = Some(
            ToolConfirmationRequest::new("record_note", serde_json::json!({"note": "hi"}))
                .with_function_call_id("call-1"),
        );
        (pending, held)
    }

    #[test]
    fn stream_printer_renders_a_held_call_with_its_reason() {
        let (pending, held) = held_call_events();
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        printer.handle_event(&pending);
        printer.handle_event(&held);
        printer.finish();

        let output = captured.text();
        assert!(
            output.contains(
                "[approval needed] record_note {\"note\":\"hi\"} — Tool 'record_note' requires \
                 confirmation: notes are permanent"
            ),
            "{output}"
        );
        assert!(!output.contains("Tool confirmation required"), "{output}");
    }

    #[test]
    fn stream_printer_renders_a_held_call_without_a_known_reason() {
        let (_, held) = held_call_events();
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        printer.handle_event(&held);
        assert_eq!(captured.text(), "\n[approval needed] record_note {\"note\":\"hi\"}\n");
    }

    #[test]
    fn stream_printer_renders_the_decision_applied_to_a_call() {
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        for (decision, response) in [
            (ToolConfirmationDecision::Approve, serde_json::json!({"recorded": "hi"})),
            (ToolConfirmationDecision::Deny, serde_json::json!({"error": "denied"})),
        ] {
            let mut event = Event::new("invocation");
            event.set_content(Content {
                role: "user".into(),
                parts: vec![Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new("record_note", response),
                    id: Some("call-1".into()),
                    annotations: None,
                }],
            });
            event.actions.tool_confirmation_decision = Some(decision);
            printer.handle_event(&event);
        }
        let output = captured.text();
        let approved = output.find("[approved] record_note").expect(&output);
        let ran = output.find("[tool-response] record_note {\"recorded\":\"hi\"}").expect(&output);
        let denied = output.find("[denied] record_note").expect(&output);
        assert!(approved < ran && ran < denied, "{output}");
    }

    #[test]
    fn replayed_provider_messages_print_once_as_text() {
        let commentary = |text: &str, status: &str| Part::ServerToolCall {
            server_tool_call: serde_json::json!({
                "type": "message", "phase": "commentary", "role": "assistant", "status": status,
                "content": [{ "type": "output_text", "text": text, "annotations": [] }],
            }),
        };
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        for (text, partial) in
            [("Checking", true), (" the code", true), ("Checking the code", false)]
        {
            let mut event = Event::new("invocation");
            event.set_content(Content {
                role: "model".into(),
                parts: vec![commentary(text, if partial { "in_progress" } else { "completed" })],
            });
            event.llm_response.partial = partial;
            printer.handle_event(&event);
        }
        let mut other = Event::new("invocation");
        other.set_content(Content {
            role: "model".into(),
            parts: vec![Part::ServerToolCall {
                server_tool_call: serde_json::json!({ "type": "web_search_call" }),
            }],
        });
        printer.handle_event(&other);
        assert_eq!(
            captured.text(),
            "Checking the code\n\n[server-tool-call] {\"type\":\"web_search_call\"}\n"
        );
    }

    #[test]
    fn a_planted_tool_name_in_a_held_call_is_escaped() {
        let mut held = Event::new("invocation");
        held.actions.tool_confirmation =
            Some(ToolConfirmationRequest::new(format!("tool{PLANTED}"), serde_json::json!({})));
        let captured = Captured::default();
        let mut printer = StreamPrinter::with_writer(ThinkingDisplayMode::Auto, captured.clone());
        printer.handle_event(&held);
        let output = captured.text();
        assert!(!output.chars().any(|c| c.is_control() && c != '\n'), "{output:?}");
        assert!(output.contains("[approval needed] tool\\u{1b}]52"), "{output:?}");
    }

    #[derive(Debug)]
    struct AllowAll;

    #[async_trait]
    impl ToolPolicy for AllowAll {
        async fn evaluate(
            &self,
            _request: &adk_core::ToolPolicyRequest,
        ) -> adk_core::PolicyDecision {
            adk_core::PolicyDecision::Allow
        }
    }

    #[derive(Debug)]
    struct Approves;

    #[async_trait]
    impl ToolConfirmationHandler for Approves {
        async fn decide(
            &self,
            _request: &ToolConfirmationRequest,
        ) -> AdkResult<ToolConfirmationDecision> {
            Ok(ToolConfirmationDecision::Approve)
        }
    }

    fn console() -> ConsoleIo {
        ConsoleIo::scripted([], true)
    }

    #[test]
    fn streaming_mode_and_run_config_merge_in_either_order() {
        let base = || RunConfig::builder().history_max_events(Some(7)).build();
        for launcher in [
            test_launcher().with_streaming_mode(StreamingMode::None).with_run_config(base()),
            test_launcher().with_run_config(base()).with_streaming_mode(StreamingMode::None),
        ] {
            let config = launcher.console_run_config(&console());
            assert_eq!(config.streaming_mode, StreamingMode::None);
            assert_eq!(config.history_max_events, Some(7));
        }
    }

    #[test]
    fn console_runs_always_have_a_handler_and_a_human_timeout() {
        let config = test_launcher().console_run_config(&console());
        assert!(config.tool_confirmation_handler.is_some());
        assert_eq!(config.tool_confirmation_timeout, CONSOLE_CONFIRMATION_TIMEOUT);
        assert!(config.tool_policy.is_none());

        let chosen = Duration::from_secs(5);
        let config = test_launcher()
            .with_run_config(RunConfig::builder().tool_confirmation_timeout(chosen).build())
            .console_run_config(&console());
        assert_eq!(config.tool_confirmation_timeout, chosen);
    }

    #[tokio::test]
    async fn the_launchers_policy_and_handler_take_precedence() {
        let policy: Arc<dyn ToolPolicy> = Arc::new(AllowAll);
        let handler: Arc<dyn ToolConfirmationHandler> = Arc::new(Approves);
        let from_config = RunConfig::builder()
            .tool_policy(Arc::new(adk_core::DeclarativePolicy::builder().build()))
            .tool_confirmation_handler(Arc::new(ConsoleConfirmationHandler::new(console())))
            .build();
        let config = test_launcher()
            .with_tool_policy(policy.clone())
            .with_confirmation_handler(handler.clone())
            .with_run_config(from_config)
            .console_run_config(&console());
        assert!(Arc::ptr_eq(config.tool_policy.as_ref().unwrap(), &policy));
        assert!(Arc::ptr_eq(config.tool_confirmation_handler.as_ref().unwrap(), &handler));
    }

    #[tokio::test]
    async fn a_session_id_resumes_the_session_or_creates_it() {
        let sessions = InMemorySessionService::new();
        let created =
            open_console_session(&sessions, "app", "ana", Some("notes".into())).await.unwrap();
        assert_eq!(created, "notes");
        sessions
            .append_event_for_identity(adk_session::AppendEventRequest {
                identity: adk_core::AdkIdentity::new(
                    adk_core::AppName::try_from("app").unwrap(),
                    UserId::new("ana").unwrap(),
                    SessionId::new("notes").unwrap(),
                ),
                event: Event::new("first-turn"),
            })
            .await
            .unwrap();

        let resumed =
            open_console_session(&sessions, "app", "ana", Some("notes".into())).await.unwrap();
        assert_eq!(resumed, "notes");
        let session = sessions
            .get(GetRequest {
                app_name: "app".into(),
                user_id: "ana".into(),
                session_id: "notes".into(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(session.events().len(), 1, "resuming keeps the conversation");

        let fresh = open_console_session(&sessions, "app", "ana", None).await.unwrap();
        assert_ne!(fresh, "notes");
    }

    struct Echo;

    #[async_trait]
    impl ConsoleCommand for Echo {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Repeat the text after the command"
        }

        async fn run(&self, args: &str, ctx: &ConsoleCommandContext) -> AdkResult<String> {
            Ok(format!("{args} ({})", ctx.session_id))
        }
    }

    #[tokio::test]
    async fn registered_commands_are_listed_and_dispatched() {
        let commands: Vec<Arc<dyn ConsoleCommand>> = vec![Arc::new(Echo)];
        let help = help_text(&commands);
        assert!(help.contains("/echo"), "{help}");
        assert!(help.contains("Repeat the text after the command"), "{help}");
        assert!(help.contains("/help"), "{help}");

        let (command, args) = find_command(&commands, "/echo  hello there ").unwrap();
        let ctx = ConsoleCommandContext::new(
            "app",
            "ana",
            "s1",
            Arc::new(InMemorySessionService::new()),
            console(),
        );
        assert_eq!(command.run(args, &ctx).await.unwrap(), "hello there (s1)");
        assert!(find_command(&commands, "/echoes").is_none());
        assert!(find_command(&commands, "echo hi").is_none());
    }

    async fn status(app: Router, method: &str, uri: &str) -> StatusCode {
        app.oneshot(Request::builder().method(method).uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn build_app_mounts_the_approval_inbox_behind_authentication() {
        let service = Arc::new(adk_core::StoreApprovalService::new(Arc::new(
            adk_core::InMemoryApprovalStore::new(),
        )));
        let app = test_launcher().with_approval_service(service).build_app().unwrap();
        assert_eq!(status(app, "GET", "/api/approvals").await, StatusCode::UNAUTHORIZED);

        let app = test_launcher().build_app().unwrap();
        assert_eq!(status(app, "GET", "/api/approvals").await, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn build_app_mounts_background_runs() {
        let app = test_launcher().with_background_runs(BackgroundState::new()).build_app().unwrap();
        assert_eq!(status(app, "GET", "/api/runs").await, StatusCode::OK);

        let app = test_launcher().build_app().unwrap();
        assert_eq!(status(app, "GET", "/api/runs").await, StatusCode::NOT_FOUND);
    }

    #[test]
    fn build_app_refuses_to_serve_without_the_launchers_tool_policy() {
        let error = test_launcher().with_tool_policy(Arc::new(AllowAll)).build_app().unwrap_err();
        assert!(error.to_string().contains("serve mode cannot apply it"), "{error}");

        let in_config = RunConfig::builder().tool_policy(Arc::new(AllowAll)).build();
        assert!(test_launcher().with_run_config(in_config).build_app().is_err());
    }

    #[tokio::test]
    async fn build_app_with_a2a_enables_agent_card_route() {
        let app = test_launcher()
            .build_app_with_a2a("http://localhost:8080")
            .expect("launcher app with a2a should build");

        let response = app
            .oneshot(Request::builder().uri("/.well-known/agent.json").body(Body::empty()).unwrap())
            .await
            .expect("agent card request should complete");

        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["name"], "launcher_test_agent");
        assert_eq!(json["description"], "launcher test agent");
    }
}
