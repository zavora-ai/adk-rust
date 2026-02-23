//! Simple launcher for ADK agents with CLI support.
//!
//! Provides a one-liner to run agents with console or web server modes,
//! similar to adk-go's launcher pattern.
//!
//! # Example
//!
//! ```ignore
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let agent = /* create your agent */;
//!
//!     // Run with CLI support (console by default, or `serve` for web)
//!     Launcher::new(agent).run().await
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
use adk_core::{Agent, AgentLoader, Result, RunConfig, StreamingMode};
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;
use clap::{Parser, Subcommand};
use std::sync::Arc;

/// CLI arguments for the launcher
#[derive(Parser)]
#[command(name = "agent")]
#[command(about = "ADK Agent", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run interactive console (default if no command specified)
    Chat,
    /// Start web server with UI
    Serve {
        /// Server port
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
}

/// Single agent loader for the launcher
pub struct SingleAgentLoader {
    agent: Arc<dyn Agent>,
}

impl SingleAgentLoader {
    /// Create a new single agent loader
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self { agent }
    }
}

#[async_trait::async_trait]
impl AgentLoader for SingleAgentLoader {
    async fn load_agent(&self, _name: &str) -> Result<Arc<dyn Agent>> {
        Ok(self.agent.clone())
    }

    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }

    fn root_agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
}

/// Launcher for running ADK agents with CLI support.
///
/// Provides console and web server modes out of the box.
pub struct Launcher {
    agent: Arc<dyn Agent>,
    app_name: Option<String>,
    artifact_service: Option<Arc<dyn ArtifactService>>,
    run_config: Option<RunConfig>,
}

impl Launcher {
    /// Create a new launcher with the given agent.
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self { agent, app_name: None, artifact_service: None, run_config: None }
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

    /// Set streaming mode (defaults to SSE if not specified).
    pub fn with_streaming_mode(mut self, mode: StreamingMode) -> Self {
        self.run_config = Some(RunConfig { streaming_mode: mode, ..RunConfig::default() });
        self
    }

    /// Run the launcher, parsing CLI arguments.
    ///
    /// - No arguments or `chat`: Interactive console
    /// - `serve [--port PORT]`: Web server with UI
    pub async fn run(self) -> Result<()> {
        let cli = Cli::parse();

        match cli.command.unwrap_or(Commands::Chat) {
            Commands::Chat => self.run_console().await,
            Commands::Serve { port } => self.run_serve(port).await,
        }
    }

    /// Run in interactive console mode.
    async fn run_console(self) -> Result<()> {
        use adk_runner::{Runner, RunnerConfig};
        use adk_session::{CreateRequest, SessionService};
        use futures::StreamExt;
        use std::collections::HashMap;
        use std::io::{self, BufRead, Write};

        let app_name = self.app_name.unwrap_or_else(|| self.agent.name().to_string());
        let user_id = "user".to_string();

        let session_service = Arc::new(InMemorySessionService::new());

        // Create session
        let session = session_service
            .create(CreateRequest {
                app_name: app_name.clone(),
                user_id: user_id.clone(),
                session_id: None,
                state: HashMap::new(),
            })
            .await?;

        let session_id = session.id().to_string();

        // Create runner
        let runner = Runner::new(RunnerConfig {
            app_name,
            agent: self.agent,
            session_service,
            artifact_service: self.artifact_service,
            memory_service: None,
            plugin_manager: None,
            run_config: self.run_config,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
        })?;

        println!("🤖 Agent ready! Type your questions (or 'exit' to quit).\n");

        let stdin = io::stdin();
        let mut stdout = io::stdout();

        loop {
            print!("You: ");
            stdout.flush()?;

            let mut input = String::new();
            let bytes_read = stdin.lock().read_line(&mut input)?;

            // EOF reached (e.g., piped input ended)
            if bytes_read == 0 {
                println!("\n👋 Goodbye!");
                break;
            }

            let input = input.trim();

            if input == "exit" || input == "quit" {
                println!("👋 Goodbye!");
                break;
            }

            if input.is_empty() {
                continue;
            }

            let content = adk_core::Content::new("user").with_text(input);
            let mut events = runner.run(user_id.clone(), session_id.clone(), content).await?;

            print!("Assistant: ");
            stdout.flush()?;

            let mut current_agent = String::new();

            while let Some(event) = events.next().await {
                match event {
                    Ok(evt) => {
                        // Track which agent is responding
                        if !evt.author.is_empty()
                            && evt.author != "user"
                            && evt.author != current_agent
                        {
                            if !current_agent.is_empty() {
                                println!();
                            }
                            current_agent = evt.author.clone();
                            println!("\n[Agent: {}]", current_agent);
                            print!("Assistant: ");
                            stdout.flush()?;
                        }

                        // Check for transfer action
                        if let Some(target) = &evt.actions.transfer_to_agent {
                            println!("\n🔄 [Transfer requested to: {}]", target);
                        }

                        if let Some(content) = evt.llm_response.content {
                            for part in content.parts {
                                if let Some(text) = part.text() {
                                    print!("{}", text);
                                    stdout.flush()?;
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("\nError: {}", e),
                }
            }
            println!("\n");
        }

        Ok(())
    }

    /// Run web server with UI.
    async fn run_serve(self, port: u16) -> Result<()> {
        // Initialize telemetry with ADK-Go style exporter
        let span_exporter = match adk_telemetry::init_with_adk_exporter("adk-server") {
            Ok(exporter) => Some(exporter),
            Err(e) => {
                eprintln!("Warning: Failed to initialize telemetry: {}", e);
                None
            }
        };

        let session_service = Arc::new(InMemorySessionService::new());
        let agent_loader = Arc::new(SingleAgentLoader::new(self.agent));

        let mut config = ServerConfig::new(agent_loader, session_service)
            .with_artifact_service_opt(self.artifact_service);

        if let Some(exporter) = span_exporter {
            config = config.with_span_exporter(exporter);
        }

        let app = create_app(config);

        let addr = format!("0.0.0.0:{}", port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        println!("🚀 ADK Server starting on http://localhost:{}", port);
        println!("📱 Open http://localhost:{} in your browser", port);
        println!("Press Ctrl+C to stop\n");

        axum::serve(listener, app).await?;

        Ok(())
    }
}
