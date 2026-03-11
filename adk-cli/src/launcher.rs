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
    RunConfig, StreamingMode,
};
use adk_runner::{Runner, RunnerConfig};
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
        self.run_config = Some(RunConfig { streaming_mode: mode, ..RunConfig::default() });
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
            .map_err(|e| adk_core::AdkError::Config(format!("failed to init readline: {e}")))?;

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
                    let runner = Runner::new(RunnerConfig {
                        app_name: app_name.clone(),
                        agent: agent.clone(),
                        session_service: session_service.clone(),
                        artifact_service: artifact_service.clone(),
                        memory_service: memory_service.clone(),
                        plugin_manager: None,
                        run_config: run_config.clone(),
                        compaction_config: None,
                        context_cache_config: None,
                        cache_capable: None,
                        request_context: None,
                        cancellation_token: Some(cancellation_token.clone()),
                    })?;
                    let mut events =
                        runner.run(user_id.clone(), session_id.clone(), user_content).await?;
                    let mut printer = StreamPrinter::new(thinking_mode);
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
                                                print!("\x1b[33m[{current_agent}]\x1b[0m ");
                                                let _ = io::stdout().flush();
                                            }
                                            printed_header = true;
                                        }

                                        // Show agent transfer requests
                                        if let Some(target) = &evt.actions.transfer_to_agent {
                                            print!("\x1b[90m[transfer -> {target}]\x1b[0m ");
                                            let _ = io::stdout().flush();
                                        }

                                        if let Some(content) = &evt.llm_response.content {
                                            for part in &content.parts {
                                                printer.handle_part(part);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        error!("stream error: {e}");
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
                if let Err(e) = adk_telemetry::init_with_otlp(service_name, endpoint) {
                    warn!("failed to initialize otlp telemetry: {e}");
                }
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
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        println!("ADK Server starting on http://localhost:{port}");
        println!("Open http://localhost:{port} in your browser");
        println!("Press Ctrl+C to stop\n");

        axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

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

/// Handles streaming output with special rendering for think blocks,
/// tool calls, function responses, and inline/file data.
pub struct StreamPrinter {
    thinking_mode: ThinkingDisplayMode,
    in_think_block: bool,
    in_thinking_part_stream: bool,
    think_buffer: String,
}

impl StreamPrinter {
    /// Create a printer with the selected thinking display mode.
    pub fn new(thinking_mode: ThinkingDisplayMode) -> Self {
        Self {
            thinking_mode,
            in_think_block: false,
            in_thinking_part_stream: false,
            think_buffer: String::new(),
        }
    }

    /// Process a single response part, rendering it to stdout.
    pub fn handle_part(&mut self, part: &Part) {
        match part {
            Part::Text { text } => {
                self.flush_part_thinking_if_needed();
                self.handle_text_chunk(text);
            }
            Part::Thinking { thinking, .. } => {
                if matches!(self.thinking_mode, ThinkingDisplayMode::Hide) {
                    return;
                }
                if !self.in_thinking_part_stream {
                    print!("\n[thinking] ");
                    let _ = io::stdout().flush();
                    self.in_thinking_part_stream = true;
                }
                self.think_buffer.push_str(thinking);
                print!("{thinking}");
                let _ = io::stdout().flush();
            }
            Part::FunctionCall { name, args, .. } => {
                self.flush_pending_thinking();
                print!("\n[tool-call] {name} {args}\n");
                let _ = io::stdout().flush();
            }
            Part::FunctionResponse { function_response, .. } => {
                self.flush_pending_thinking();
                self.print_tool_response(&function_response.name, &function_response.response);
            }
            Part::InlineData { mime_type, data } => {
                self.flush_pending_thinking();
                print!("\n[inline-data] mime={mime_type} bytes={}\n", data.len());
                let _ = io::stdout().flush();
            }
            Part::FileData { mime_type, file_uri } => {
                self.flush_pending_thinking();
                print!("\n[file-data] mime={mime_type} uri={file_uri}\n");
                let _ = io::stdout().flush();
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
                print!("{visible}");
                let _ = io::stdout().flush();
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
                    print!("{visible}");
                    let _ = io::stdout().flush();
                }
                self.in_think_block = true;
                self.think_buffer.clear();
                remaining = &remaining[start_idx + THINK_START.len()..];
            } else {
                print!("{remaining}");
                let _ = io::stdout().flush();
                break;
            }
        }
    }

    fn flush_think(&mut self) {
        let content = self.think_buffer.trim();
        if !content.is_empty() {
            print!("\n[think] {content}\n");
            let _ = io::stdout().flush();
        }
        self.think_buffer.clear();
    }

    /// Flush any pending think block content.
    pub fn finish(&mut self) {
        self.flush_pending_thinking();
    }

    fn print_tool_response(&self, name: &str, response: &Value) {
        print!("\n[tool-response] {name} {response}\n");
        let _ = io::stdout().flush();
    }

    fn flush_part_thinking_if_needed(&mut self) {
        if self.in_thinking_part_stream {
            println!();
            let _ = io::stdout().flush();
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
            print!("\n[{label}] {content}\n");
            let _ = io::stdout().flush();
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
            function_response: adk_core::FunctionResponseData {
                name: "get_weather".into(),
                response: serde_json::json!({"temp": 72}),
            },
            id: None,
        });

        // InlineData
        printer
            .handle_part(&Part::InlineData { mime_type: "image/png".into(), data: vec![0u8; 100] });

        // FileData
        printer.handle_part(&Part::FileData {
            mime_type: "audio/wav".into(),
            file_uri: "gs://bucket/file.wav".into(),
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
