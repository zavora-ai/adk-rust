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
//! # Web server with UI
//! cargo run -- serve
//! cargo run -- serve --port 3000
//! ```

use adk_artifact::ArtifactService;
use adk_core::{
    Agent, CacheCapable, Content, ContextCacheConfig, EventsCompactionConfig, Memory, Part, Result,
    RunConfig, SessionId, StreamingMode, UserId,
};
use adk_runner::Runner;
use adk_server::{
    RequestContextExtractor, SecurityConfig, ServerConfig, create_app, create_app_with_a2a,
    shutdown_signal,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use axum::Router;
use clap::{Parser, Subcommand};
use futures::StreamExt;
use rustyline::DefaultEditor;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

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
/// Starts an Axum HTTP server with the ADK web UI.
///
/// # Note on `with_streaming_mode`
///
/// `with_streaming_mode` currently only affects console mode. `ServerConfig`
/// does not accept a `RunConfig`, so the setting is not forwarded to the
/// server. This will be addressed when `ServerConfig` gains that field.
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
    thinking_mode: ThinkingDisplayMode,
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
            thinking_mode: ThinkingDisplayMode::Auto,
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
    /// Note: this currently only affects console mode. The server does not
    /// yet accept a `RunConfig`.
    pub fn with_streaming_mode(mut self, mode: StreamingMode) -> Self {
        self.run_config = Some(RunConfig::builder().streaming_mode(mode).build());
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
    /// - `serve [--port PORT]`: Web server with UI
    pub async fn run(self) -> Result<()> {
        let cli = LauncherCli::parse();

        match cli.command.unwrap_or(LauncherCommand::Chat) {
            LauncherCommand::Chat => self.run_console_directly().await,
            LauncherCommand::Serve { port } => self.run_serve_directly(port).await,
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
        let app_name = self.app_name.unwrap_or_else(|| self.agent.name().to_string());
        let user_id = "user".to_string();
        let thinking_mode = self.thinking_mode;
        let agent = self.agent;
        let artifact_service = self.artifact_service;
        let memory_service = self.memory_service;
        let run_config = self.run_config;

        let session_service =
            self.session_service.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));

        let session = session_service
            .create(CreateRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: None,
                state: HashMap::new(),
            })
            .await?;

        let session_id = session.id().to_string();

        let mut rl = DefaultEditor::new()
            .map_err(|e| adk_core::AdkError::config(format!("failed to init readline: {e}")))?;

        print_banner(agent.name());

        loop {
            let readline = rl.readline("\x1b[36mYou >\x1b[0m ");
            match readline {
                Ok(line) => {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if is_exit_command(&trimmed) {
                        println!("\nGoodbye.\n");
                        break;
                    }
                    if trimmed == "/help" {
                        print_help();
                        continue;
                    }
                    if trimmed == "/clear" {
                        println!("(conversation cleared — session state is unchanged)");
                        continue;
                    }

                    let _ = rl.add_history_entry(&line);

                    let user_content = Content::new("user").with_text(trimmed);
                    println!();

                    let cancellation_token = CancellationToken::new();
                    let mut runner_builder = Runner::builder()
                        .app_name(app_name.clone())
                        .agent(agent.clone())
                        .session_service(session_service.clone())
                        .cancellation_token(cancellation_token.clone());
                    if let Some(ref artifact_service) = artifact_service {
                        runner_builder = runner_builder.artifact_service(artifact_service.clone());
                    }
                    if let Some(ref memory_service) = memory_service {
                        runner_builder = runner_builder.memory_service(memory_service.clone());
                    }
                    if let Some(ref run_config) = run_config {
                        runner_builder = runner_builder.run_config(run_config.clone());
                    }
                    let runner = runner_builder.build()?;
                    let mut events = runner
                        .run(
                            UserId::new(user_id.clone())?,
                            SessionId::new(session_id.clone())?,
                            user_content,
                        )
                        .await?;
                    let mut printer = StreamPrinter::new(thinking_mode);
                    let mut text_deltas = adk_core::EventTextDeltas::default();
                    let mut current_agent = String::new();
                    let mut printed_header = false;
                    let mut interrupted = false;

                    loop {
                        tokio::select! {
                            event = events.next() => {
                                let Some(event) = event else {
                                    break;
                                };

                                match event {
                                    Ok(evt) => {
                                        let evt = text_deltas.push(&evt);
                                        // Track agent switches in multi-agent workflows
                                        if !evt.author.is_empty()
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
                                        if let Some(target) = &evt.actions.transfer_to_agent {
                                            print!(
                                                "\x1b[90m[transfer -> {}]\x1b[0m ",
                                                terminal_safe(target)
                                            );
                                            let _ = io::stdout().flush();
                                        }

                                        if let Some(content) = &evt.llm_response.content {
                                            for part in &content.parts {
                                                printer.handle_part(part);
                                            }
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
                            signal = tokio::signal::ctrl_c() => {
                                match signal {
                                    Ok(()) => {
                                        cancellation_token.cancel();
                                        interrupted = true;
                                        break;
                                    }
                                    Err(err) => {
                                        error!("failed to listen for Ctrl+C: {err}");
                                    }
                                }
                            }
                        }
                    }

                    printer.finish();
                    if interrupted {
                        println!("\nInterrupted.\n");
                        continue;
                    }

                    println!("\n");
                }
                Err(rustyline::error::ReadlineError::Interrupted) => {
                    println!("\nInterrupted. Type exit to quit.\n");
                    continue;
                }
                Err(rustyline::error::ReadlineError::Eof) => {
                    println!("\nGoodbye.\n");
                    break;
                }
                Err(err) => {
                    error!("readline error: {err}");
                    break;
                }
            }
        }

        Ok(())
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
    /// middleware, metrics, or owning the serve loop yourself.
    pub fn build_app(self) -> Result<Router> {
        let span_exporter = self.init_telemetry();
        let a2a_base_url = self.a2a_base_url.clone();
        let config = self.into_server_config(span_exporter);

        Ok(match a2a_base_url {
            Some(base_url) => create_app_with_a2a(config, Some(&base_url)),
            None => create_app(config),
        })
    }

    /// Build the Axum application with A2A routes enabled.
    pub fn build_app_with_a2a(mut self, base_url: impl Into<String>) -> Result<Router> {
        self.a2a_base_url = Some(base_url.into());
        self.build_app()
    }

    /// Run web server without parsing CLI arguments.
    ///
    /// Use this when you already know you want serve mode (e.g. from
    /// your own CLI parser). [`run`](Self::run) calls this internally.
    pub async fn run_serve_directly(self, port: u16) -> Result<()> {
        let app = self.build_app()?;

        let addr = format!("0.0.0.0:{port}");
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| adk_core::AdkError::config(format!("failed to bind {addr}: {e}")))?;

        println!("ADK Server starting on http://localhost:{port}");
        println!("Open http://localhost:{port} in your browser");
        println!("Press Ctrl+C to stop\n");

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(|e| adk_core::AdkError::config(format!("server error: {e}")))?;

        Ok(())
    }
}

/// Print the ADK-Rust welcome banner.
fn print_banner(agent_name: &str) {
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
    println!("  Runtime  : Tokio async, streaming responses");
    println!("  Features : tool calling, multi-provider, multi-agent, think blocks");
    println!();
    println!("  Type a message to chat. /help for commands.");
    println!();
}

/// Print available REPL commands.
fn print_help() {
    println!();
    println!("  Commands:");
    println!("    /help   Show this help");
    println!("    /clear  Clear display (session state is kept)");
    println!("    quit    Exit the REPL");
    println!("    exit    Exit the REPL");
    println!("    /quit   Exit the REPL");
    println!("    /exit   Exit the REPL");
    println!();
    println!("  Tips:");
    println!("    - Up/Down arrows browse history");
    println!("    - Ctrl+C interrupts the current operation");
    println!("    - Multi-agent workflows show [agent_name] on handoff");
    println!();
}

fn is_exit_command(input: &str) -> bool {
    matches!(input, "quit" | "exit" | "/quit" | "/exit")
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
pub struct StreamPrinter<W: Write = io::Stdout> {
    thinking_mode: ThinkingDisplayMode,
    in_think_block: bool,
    in_thinking_part_stream: bool,
    think_buffer: String,
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
            out,
        }
    }

    /// Writes `text` with terminal control characters escaped, then flushes.
    fn emit(&mut self, text: &str) {
        let _ = self.out.write_all(terminal_safe(text).as_bytes());
        let _ = self.out.flush();
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
                self.emit(&format!("\n[server-tool-call] {server_tool_call}\n"));
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
