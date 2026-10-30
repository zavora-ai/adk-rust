//! # Agent Development Kit (ADK) for Rust
//!
//! [![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
//! [![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
//! [![License](https://img.shields.io/crates/l/adk-rust.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)
//!
//! A flexible and modular framework for developing and deploying AI agents in Rust.
//! ADK-Rust is model-agnostic and deployment-agnostic: this umbrella crate re-exports the
//! ADK-Rust crates behind feature flags grouped into four tiers.
//!
//! ## Quick Start
//!
//! Create your first AI agent in minutes:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
//!     let api_key = std::env::var("GOOGLE_API_KEY")?;
//!     let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;
//!
//!     let agent = LlmAgentBuilder::new("assistant")
//!         .description("A helpful AI assistant")
//!         .instruction("You are a friendly assistant. Answer questions concisely.")
//!         .model(Arc::new(model))
//!         .build()?;
//!
//!     // Run in interactive console mode
//!     Launcher::new(Arc::new(agent)).run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Installation
//!
//! Add to your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! adk-rust = "3.0.0"
//! tokio = { version = "1.40", features = ["full"] }
//! dotenvy = "0.15"  # For loading .env files
//! ```
//!
//! ### Feature Presets
//!
//! ```toml
//! # Minimal (default) — agents, Gemini, runner, sessions (fastest build)
//! adk-rust = "3.0.0"
//!
//! # Standard — minimal + OpenAI, Anthropic (and the adk-anthropic client), skills, graph,
//! # auth, server, eval, guardrail, plugin, artifacts, tools, memory, telemetry, computer-use
//! adk-rust = { version = "3.0.0", features = ["standard"] }
//!
//! # Enterprise — standard + realtime, browser, rag, payments, awp
//! adk-rust = { version = "3.0.0", features = ["enterprise"] }
//!
//! # Full — enterprise + audio, code, sandbox, code-tools, the harness and its kernel, and
//! # the graph-functional, graph-node-cache, graph-delta, and graph-time-travel capabilities
//! adk-rust = { version = "3.0.0", features = ["full"] }
//!
//! # Custom — pick exactly what you need
//! adk-rust = { version = "3.0.0", default-features = false, features = [
//!     "agents", "gemini", "tools", "sessions", "openai", "openrouter"
//! ] }
//! ```
//!
//! ## Agent Types
//!
//! ADK-Rust provides several agent types for different use cases:
//!
//! ### LlmAgent - AI-Powered Reasoning
//!
//! The core agent type that uses Large Language Models for intelligent
//! reasoning. `GoogleSearchTool` below requires the `tools` feature (included in
//! `standard` and above); the agent itself needs only the default tier:
//!
//! ```no_run
//! # #[cfg(feature = "tools")]
//! # async fn example() -> adk_rust::Result<()> {
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! let api_key = std::env::var("GOOGLE_API_KEY").map_err(|e| AdkError::config(e.to_string()))?;
//! let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;
//!
//! let agent = LlmAgentBuilder::new("researcher")
//!     .description("Research assistant with web search")
//!     .instruction("Search for information and provide detailed summaries.")
//!     .model(Arc::new(model))
//!     .tool(Arc::new(GoogleSearchTool::new()))  // Add tools
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Workflow Agents - Deterministic Pipelines
//!
//! For predictable, multi-step workflows:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<()> {
//! # let researcher: Arc<dyn Agent> = todo!();
//! # let writer: Arc<dyn Agent> = todo!();
//! # let reviewer: Arc<dyn Agent> = todo!();
//! // Sequential: Execute agents in order
//! let pipeline = SequentialAgent::new(
//!     "content_pipeline",
//!     vec![researcher, writer, reviewer]
//! );
//!
//! // Parallel: Execute agents concurrently
//! # let analyst1: Arc<dyn Agent> = todo!();
//! # let analyst2: Arc<dyn Agent> = todo!();
//! let parallel = ParallelAgent::new(
//!     "multi_analysis",
//!     vec![analyst1, analyst2]
//! );
//!
//! // Loop: Iterate until condition met
//! # let refiner: Arc<dyn Agent> = todo!();
//! let loop_agent = LoopAgent::new("iterative_refiner", vec![refiner])
//!     .with_max_iterations(5);
//! # Ok(())
//! # }
//! ```
//!
//! ### Multi-Agent Systems
//!
//! Build hierarchical agent systems with automatic delegation:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<()> {
//! # let model: Arc<dyn Llm> = todo!();
//! # let code_agent: Arc<dyn Agent> = todo!();
//! # let test_agent: Arc<dyn Agent> = todo!();
//! let coordinator = LlmAgentBuilder::new("coordinator")
//!     .description("Development team coordinator")
//!     .instruction("Delegate coding tasks to specialists.")
//!     .model(model)
//!     .sub_agent(code_agent)   // Delegate to sub-agents
//!     .sub_agent(test_agent)
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Tools
//!
//! Give your agents capabilities beyond conversation. The tool types below
//! require the `tools` feature (included in `standard` and above):
//!
//! ### Function Tools - Custom Operations
//!
//! Convert any async function into a tool:
//!
//! ```no_run
//! # #[cfg(feature = "tools")]
//! # fn example() -> adk_rust::Result<()> {
//! use adk_rust::prelude::*;
//! use adk_rust::serde_json::{json, Value};
//! use std::sync::Arc;
//!
//! async fn get_weather(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
//!     let city = args["city"].as_str().unwrap_or("Unknown");
//!     // Your weather API call here
//!     Ok(json!({
//!         "temperature": 72.0,
//!         "conditions": "Sunny",
//!         "city": city
//!     }))
//! }
//!
//! let weather_tool = FunctionTool::new(
//!     "get_weather",
//!     "Get current weather for a city",
//!     get_weather,
//! );
//! # Ok(())
//! # }
//! ```
//!
//! ### Built-in Tools
//!
//! Ready-to-use tools included with ADK:
//!
//! - `GoogleSearchTool` - Web search via Google
//! - `ExitLoopTool` - Control loop termination
//! - `LoadArtifactsTool` - Access stored artifacts
//!
//! ### MCP Tools - External Integrations
//!
//! Connect to Model Context Protocol servers using the `rmcp` crate:
//!
//! ```ignore
//! use adk_rust::prelude::*;
//! use adk_rust::tool::McpToolset;
//! use rmcp::{ServiceExt, transport::TokioChildProcess};
//! use tokio::process::Command;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to an MCP server (e.g., filesystem, database)
//! let client = ().serve(TokioChildProcess::new(
//!     Command::new("npx")
//!         .arg("-y")
//!         .arg("@modelcontextprotocol/server-filesystem")
//!         .arg("/path/to/dir")
//! )?).await?;
//!
//! let mcp_tools = McpToolset::new(client);
//!
//! // Add all MCP tools to your agent
//! # let builder: LlmAgentBuilder = todo!();
//! let agent = builder.toolset(Arc::new(mcp_tools)).build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Sessions & State
//!
//! Manage conversation context and working memory:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use adk_rust::session::{SessionService, CreateRequest};
//! use adk_rust::serde_json::json;
//! use std::collections::HashMap;
//!
//! # async fn example() -> Result<()> {
//! let session_service = InMemorySessionService::new();
//!
//! // Create a session
//! let session = session_service.create(CreateRequest {
//!     app_name: "my_app".to_string(),
//!     user_id: "user_123".to_string(),
//!     session_id: None,
//!     state: HashMap::new(),
//! }).await?;
//!
//! // Read state (State trait provides read access)
//! let state = session.state();
//! let config = state.get("app:config");  // Returns Option<Value>
//! # Ok(())
//! # }
//! ```
//!
//! ## Callbacks
//!
//! Intercept and customize agent behavior:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<()> {
//! # let model: Arc<dyn Llm> = todo!();
//! let agent = LlmAgentBuilder::new("monitored_agent")
//!     .model(model)
//!     // Modify or inspect model responses
//!     .after_model_callback(Box::new(|_ctx, response| {
//!         Box::pin(async move {
//!             println!("Model responded");
//!             Ok(Some(response)) // Return modified response or None to keep original
//!         })
//!     }))
//!     // Track tool usage
//!     .before_tool_callback(Box::new(|_ctx| {
//!         Box::pin(async move {
//!             println!("Tool about to be called");
//!             Ok(None) // Continue execution
//!         })
//!     }))
//!     .build()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Artifacts
//!
//! Store and retrieve binary data (images, files, etc.). Requires the
//! `artifacts` feature (included in `standard` and above):
//!
//! ```no_run
//! # #[cfg(feature = "artifacts")]
//! # async fn example() -> adk_rust::Result<()> {
//! use adk_rust::prelude::*;
//! use adk_rust::artifact::{ArtifactService, SaveRequest, LoadRequest};
//!
//! let artifact_service = InMemoryArtifactService::new();
//!
//! // Save an artifact
//! let response = artifact_service.save(SaveRequest {
//!     app_name: "my_app".to_string(),
//!     user_id: "user_123".to_string(),
//!     session_id: "session_456".to_string(),
//!     file_name: "sales_chart.png".to_string(),
//!     part: Part::Text { text: "chart data".to_string() },
//!     version: None,
//! }).await?;
//!
//! // Load an artifact
//! let loaded = artifact_service.load(LoadRequest {
//!     app_name: "my_app".to_string(),
//!     user_id: "user_123".to_string(),
//!     session_id: "session_456".to_string(),
//!     file_name: "sales_chart.png".to_string(),
//!     version: None,
//! }).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Deployment Options
//!
//! ### Console Mode (Interactive CLI)
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<()> {
//! # let agent: Arc<dyn Agent> = todo!();
//! // Interactive chat in terminal
//! Launcher::new(agent).run().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Server Mode (REST API)
//!
//! With the `cli` feature, [`Launcher`] is `adk_cli::Launcher`, which also serves the agent
//! over HTTP on `127.0.0.1`:
//!
//! ```bash
//! cargo run -- serve --port 8080
//! ```
//!
//! The server exposes `GET /api/health`, the session endpoints under `/api/sessions` and
//! `/api/apps/{app}/users/{user}/sessions`, `POST /api/run/{app}/{user}/{session}` (SSE),
//! and `POST /api/run_sse`. See `adk-server` for the full list. The web home of `adk-surface`
//! (feature `surface`) is served by the harness launcher, or mounted on a server you build
//! (spec 3.3 §5.1).
//!
//! ### Agent-to-Agent (A2A) Protocol
//!
//! Expose your agent for inter-agent communication. Requires the `server`
//! feature (included in `standard` and above):
//!
//! ```no_run
//! # #[cfg(feature = "server")]
//! # async fn example() -> adk_rust::Result<()> {
//! use adk_rust::server::{create_app_with_a2a, ServerConfig};
//! use adk_rust::AgentLoader;
//!
//! # let agent_loader: std::sync::Arc<dyn AgentLoader> = todo!();
//! # let session_service: std::sync::Arc<dyn adk_rust::session::SessionService> = todo!();
//! // Create server with A2A protocol support
//! let config = ServerConfig::new(agent_loader, session_service);
//! let app = create_app_with_a2a(config, Some("http://localhost:8080"));
//!
//! // Run the server (requires axum dependency)
//! // let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
//! // axum::serve(listener, app).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Observability
//!
//! Built-in OpenTelemetry support for production monitoring. Requires the
//! `telemetry` feature (included in `standard` and above); OTLP export adds
//! `telemetry-otlp`:
//!
//! ```no_run
//! # #[cfg(feature = "telemetry")]
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use adk_rust::telemetry::init_telemetry;
//!
//! // Basic telemetry with console logging
//! init_telemetry("my-agent-service")?;
//!
//! // With `features = ["telemetry-otlp"]`, export spans to a collector instead:
//! // adk_rust::telemetry::init_with_otlp("my-agent-service", "http://localhost:4317")?;
//!
//! // All agent operations now emit traces and metrics
//! # Ok(())
//! # }
//! ```
//!
//! ## Architecture
//!
//! ADK-Rust uses a layered architecture for modularity:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    Application Layer                        │
//! │              Launcher • REST Server • A2A                   │
//! ├─────────────────────────────────────────────────────────────┤
//! │                      Runner Layer                           │
//! │           Agent Execution • Event Streaming                 │
//! ├─────────────────────────────────────────────────────────────┤
//! │                      Agent Layer                            │
//! │    LlmAgent • CustomAgent • Sequential • Parallel • Loop    │
//! ├─────────────────────────────────────────────────────────────┤
//! │                     Service Layer                           │
//! │      Models • Tools • Sessions • Artifacts • Memory         │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Feature Flags
//!
//! | Feature | Description | Preset |
//! |---------|-------------|--------|
//! | `agents` | Agent implementations | minimal (default) |
//! | `models` | Model integrations | minimal (default) |
//! | `gemini` | Gemini model support | minimal (default) |
//! | `runner` | Execution runtime | minimal (default) |
//! | `sessions` | Session management | minimal (default) |
//! | `openai` | OpenAI model support | standard |
//! | `anthropic` / `anthropic-client` | Anthropic model support / the `adk-anthropic` client | standard |
//! | `tools` | Tool system | standard |
//! | `skills` | Skill discovery | standard |
//! | `artifacts` | Artifact storage | standard |
//! | `memory` | Semantic memory | standard |
//! | `telemetry` | OpenTelemetry | standard |
//! | `guardrail` | Input/output validation | standard |
//! | `auth` | Access control | standard |
//! | `plugin` | Plugin system | standard |
//! | `server` | HTTP server + A2A | standard |
//! | `graph` | Graph workflows | standard |
//! | `eval` | Agent evaluation | standard |
//! | `computer-use` | Governed desktop-automation orchestration | standard |
//! | `realtime` | Voice/audio streaming | enterprise |
//! | `browser` | Browser automation | enterprise |
//! | `rag` | RAG pipeline | enterprise |
//! | `payments` | Agentic commerce (ACP/AP2) | enterprise |
//! | `awp` | Agentic Web Protocol | enterprise |
//! | `code`, `sandbox`, `code-tools` | Code execution and sandboxed execution | full |
//! | `audio` | Audio processing | full |
//! | `graph-functional`, `graph-node-cache`, `graph-delta`, `graph-time-travel` | Graph capabilities | full |
//! | `harness` | The governed, durable harness agent (`adk-harness`) | full |
//! | `harness-kernel` | The agent kernel, the harness's always-on form (`adk-kernel`) | full |
//! | `harness-browser`, `harness-computer-use`, `harness-payments`, `harness-voice`, `harness-managed` | Harness packs | opt-in |
//! | `cli` | CLI launcher with serve mode | opt-in |
//! | `managed-runtime` | Managed agent runtime (`adk-managed`) | opt-in |
//! | `enterprise-client` | Enterprise client SDK (`adk-enterprise`) | opt-in |
//! | `gemini-agent-platform` | Every Gemini Enterprise Agent Platform integration except realtime transports and host-side deploy tooling | opt-in |
//! | `gemini-agent-platform-full` | `gemini-agent-platform` + Vertex AI Live API (realtime stack) | opt-in |
//! | `gcp-deploy` | Agent Engine deployment client (host-side; not part of `gemini-agent-platform`) | opt-in |
//!
//! Production backends (`postgres-session`, `redis-memory`, ...), additional providers,
//! and specialist integrations are opt-in features as well; `Cargo.toml` lists every one.
//!
//! ## Examples
//!
//! The [examples directory](https://github.com/zavora-ai/adk-rust/tree/main/examples)
//! contains working examples for every feature:
//!
//! - **Agents**: LLM agent, workflow agents, multi-agent systems
//! - **Tools**: Function tools, Google Search, MCP integration
//! - **Sessions**: State management, conversation history
//! - **Callbacks**: Logging, guardrails, caching
//! - **Deployment**: Console, server, A2A protocol
//!
//! ## Related Crates
//!
//! ADK-Rust is composed of modular crates that can be used independently:
//!
//! - [`adk-core`](https://docs.rs/adk-core) - Core traits and types
//! - [`adk-agent`](https://docs.rs/adk-agent) - Agent implementations
//! - [`adk-model`](https://docs.rs/adk-model) - LLM integrations
//! - [`adk-tool`](https://docs.rs/adk-tool) - Tool system
//! - [`adk-session`](https://docs.rs/adk-session) - Session management
//! - [`adk-artifact`](https://docs.rs/adk-artifact) - Artifact storage
//! - [`adk-runner`](https://docs.rs/adk-runner) - Execution runtime
//! - [`adk-server`](https://docs.rs/adk-server) - HTTP server
//! - [`adk-telemetry`](https://docs.rs/adk-telemetry) - Observability

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// ============================================================================
// Core (always available)
// ============================================================================

/// Core traits and types.
///
/// Always available regardless of feature flags. Includes:
/// - [`Agent`] - The fundamental trait for all agents
/// - [`Tool`] / [`Toolset`] - For extending agents with capabilities
/// - [`Session`] / [`State`] - For managing conversation context
/// - [`Event`] - For streaming agent responses
/// - [`AdkError`] / [`Result`] - Unified error handling
pub use adk_core::*;

// Re-export common dependencies for convenience
pub use anyhow;
pub use async_trait::async_trait;
pub use futures;
pub use serde;
pub use serde_json;
pub use tokio;

// ============================================================================
// Component Modules (feature-gated)
// ============================================================================

/// Agent implementations (LLM, Custom, Workflow agents).
///
/// Provides the core agent types:
/// - [`LlmAgent`](agent::LlmAgent) - AI-powered agent using LLMs
/// - [`CustomAgent`](agent::CustomAgent) - Implement custom agent logic
/// - [`SequentialAgent`](agent::SequentialAgent) - Execute agents in sequence
/// - [`ParallelAgent`](agent::ParallelAgent) - Execute agents concurrently
/// - [`LoopAgent`](agent::LoopAgent) - Iterative execution until condition met
///
/// Available with feature: `agents`
#[cfg(feature = "agents")]
#[cfg_attr(docsrs, doc(cfg(feature = "agents")))]
pub mod agent {
    pub use adk_agent::*;
}

/// Model integrations (Gemini, etc.).
///
/// Provides LLM implementations:
/// - [`GeminiModel`](model::GeminiModel) - Google's Gemini models
///
/// ADK is model-agnostic - implement the [`Llm`] trait for other providers.
///
/// Available with feature: `models`
#[cfg(feature = "models")]
#[cfg_attr(docsrs, doc(cfg(feature = "models")))]
pub mod model {
    pub use adk_model::*;
}

/// Tool system and built-in tools.
///
/// Give agents capabilities beyond conversation:
/// - `FunctionTool` - Wrap async functions as tools
/// - `GoogleSearchTool` - Web search
/// - `ExitLoopTool` - Control loop agents
/// - `McpToolset` - MCP server integration with the `mcp` feature
/// - `CodeTool` / `PythonCodeTool` / `JavaScriptCodeTool` / `MontyPythonCodeTool` -
///   Code execution with the `code-tools` feature (included in `full`); the
///   embedded live paths need `code-embedded-js` / `code-embedded-python`
///
/// Available with feature: `tools`
#[cfg(feature = "tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
pub mod tool {
    pub use adk_tool::*;
}

/// AgentSkills parsing, indexing, and runtime injection helpers.
///
/// Provides:
/// - Skill file discovery from `.skills/`
/// - Frontmatter validation (`name`, `description`)
/// - Lexical skill selection
/// - Runner plugin helper for skill injection
///
/// Available with feature: `skills`
#[cfg(feature = "skills")]
#[cfg_attr(docsrs, doc(cfg(feature = "skills")))]
pub mod skill {
    pub use adk_skill::*;
}

/// Session management.
///
/// Manage conversation context and state:
/// - [`InMemorySessionService`](session::InMemorySessionService) - In-memory sessions
/// - Session creation, retrieval, and lifecycle
/// - State management with scoped prefixes
///
/// Available with feature: `sessions`
#[cfg(feature = "sessions")]
#[cfg_attr(docsrs, doc(cfg(feature = "sessions")))]
pub mod session {
    pub use adk_session::*;
}

/// Artifact storage.
///
/// Store and retrieve binary data:
/// - [`InMemoryArtifactService`](artifact::InMemoryArtifactService) - In-memory storage
/// - Version tracking for artifacts
/// - Namespace scoping
///
/// Available with feature: `artifacts`
#[cfg(feature = "artifacts")]
#[cfg_attr(docsrs, doc(cfg(feature = "artifacts")))]
pub mod artifact {
    pub use adk_artifact::*;
}

/// Memory system with semantic search.
///
/// Long-term memory for agents:
/// - [`InMemoryMemoryService`](memory::InMemoryMemoryService) - In-memory storage
/// - Semantic search capabilities
/// - Memory retrieval and updates
///
/// Available with feature: `memory`
#[cfg(feature = "memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "memory")))]
pub mod memory {
    pub use adk_memory::*;
}

/// Agent execution runtime.
///
/// The engine that manages agent execution:
/// - [`Runner`](runner::Runner) - Executes agents with full context
/// - [`RunnerConfig`](runner::RunnerConfig) - Configuration options
/// - Event streaming and tool coordination
///
/// Available with feature: `runner`
#[cfg(feature = "runner")]
#[cfg_attr(docsrs, doc(cfg(feature = "runner")))]
pub mod runner {
    pub use adk_runner::*;
}

/// HTTP server (REST + A2A).
///
/// Deploy agents as web services:
/// - REST API for chat interactions
/// - A2A (Agent-to-Agent) protocol support
/// - Routes of your own beside them, such as the web home of `adk-surface`
///
/// Available with feature: `server`
#[cfg(feature = "server")]
#[cfg_attr(docsrs, doc(cfg(feature = "server")))]
pub mod server {
    pub use adk_server::*;
}

/// Telemetry (OpenTelemetry integration).
///
/// Production observability:
/// - Distributed tracing
/// - Metrics collection
/// - Log correlation
///
/// Available with feature: `telemetry`
#[cfg(feature = "telemetry")]
#[cfg_attr(docsrs, doc(cfg(feature = "telemetry")))]
pub mod telemetry {
    pub use adk_telemetry::*;
}

/// Deployment tooling (Agent Engine BYOC deployment client).
///
/// Host-side utilities for deploying agents:
/// - `deploy::gcp` — create, poll, get, and delete ReasoningEngines
///
/// Available with feature: `gcp-deploy`
#[cfg(feature = "gcp-deploy")]
#[cfg_attr(docsrs, doc(cfg(feature = "gcp-deploy")))]
pub mod deploy {
    pub use adk_deploy::*;
}

/// Graph-based workflow engine (LangGraph-inspired).
///
/// Build complex agent workflows with:
/// - [`StateGraph`](graph::StateGraph) - Graph builder with nodes and edges
/// - [`GraphAgent`](graph::GraphAgent) - ADK Agent integration
/// - [`Checkpointer`](graph::Checkpointer) - Persistent state for human-in-the-loop
/// - [`Router`](graph::Router) - Conditional edge routing helpers
/// - Cycle support with recursion limits
/// - Streaming execution modes
///
/// Available with feature: `graph`
#[cfg(feature = "graph")]
#[cfg_attr(docsrs, doc(cfg(feature = "graph")))]
pub mod graph {
    pub use adk_graph::*;
}

/// Graph, auth, and v8 wire contracts for safe computer-use orchestration.
///
/// Available with feature: `computer-use` (included in `standard`).
#[cfg(feature = "computer-use")]
#[cfg_attr(docsrs, doc(cfg(feature = "computer-use")))]
pub mod computer_use {
    pub use adk_computer_use::*;
}

/// Agent Client Protocol: external ACP agents as tools, and ADK agents as ACP agents.
///
/// - [`AcpAgentTool`](acp::AcpAgentTool) - An external ACP agent (Claude Code, Codex,
///   Kiro CLI) as a tool
/// - [`AcpSession`](acp::AcpSession) - A persistent session with an external ACP agent
/// - [`agent_client_protocol`](acp::agent_client_protocol) - The official ACP SDK
///
/// Exposing an ADK agent over ACP needs `adk-acp`'s `server` feature.
///
/// Available with feature: `acp`
///
/// # Example
///
/// ```rust
/// use adk_rust::acp::AcpAgentTool;
///
/// let reviewer = AcpAgentTool::new("claude-code --acp")
///     .name("code_reviewer")
///     .description("Reviews a change and reports problems");
/// ```
#[cfg(feature = "acp")]
#[cfg_attr(docsrs, doc(cfg(feature = "acp")))]
pub mod acp {
    pub use adk_acp::*;
}

/// Agentic Web Protocol: discovery, capability manifests, trust, consent, and events for
/// agent-facing websites.
///
/// Available with feature: `awp`
#[cfg(feature = "awp")]
#[cfg_attr(docsrs, doc(cfg(feature = "awp")))]
pub mod awp {
    pub use adk_awp::*;
}

/// Code execution substrate (experimental — `full` preset).
///
/// First-class code execution for agents, Studio, and generated projects:
/// - [`CodeExecutor`](code::CodeExecutor) - Backend trait for execution
/// - [`ExecutionRequest`](code::ExecutionRequest) - Typed execution request
/// - [`ExecutionResult`](code::ExecutionResult) - Structured execution result
/// - [`SandboxPolicy`](code::SandboxPolicy) - Sandbox capability model
/// - [`Workspace`](code::Workspace) - Collaborative project context
///
/// Available with feature: `code`
#[cfg(feature = "code")]
#[cfg_attr(docsrs, doc(cfg(feature = "code")))]
pub mod code {
    pub use adk_code::*;
}

/// Python `CodeRuntime` for the CodeActAgent, backed by the Monty interpreter
/// (experimental).
///
/// Lets a [`CodeActAgent`](agent::codeact::CodeActAgent) act by writing Python:
/// - [`MontyRuntime`](codeact_monty::MontyRuntime) - The `CodeRuntime` implementation
/// - [`OsAccess`](codeact_monty::OsAccess) - Host-granted filesystem/environment/clock policy
/// - [`PathAccess`](codeact_monty::PathAccess) - Read-only / read-write mount modes
///
/// Available with feature: `codeact-monty` (implies `codeact`)
#[cfg(feature = "codeact-monty")]
#[cfg_attr(docsrs, doc(cfg(feature = "codeact-monty")))]
pub mod codeact_monty {
    pub use adk_codeact_monty::*;
}

/// Isolated code execution runtime (experimental — `full` preset).
///
/// Provides the [`SandboxBackend`](sandbox::SandboxBackend) trait and built-in backends:
/// - [`ProcessBackend`](sandbox::ProcessBackend) - Subprocess execution with timeout and env isolation
/// - `WasmBackend` - In-process WASM execution via wasmtime (requires `wasm` feature)
/// - [`SandboxTool`](sandbox::SandboxTool) - Tool trait implementation for agent integration
///
/// Available with feature: `sandbox`
#[cfg(feature = "sandbox")]
#[cfg_attr(docsrs, doc(cfg(feature = "sandbox")))]
pub mod sandbox {
    pub use adk_sandbox::*;
}

/// Lightweight console launcher — always available with the `runner` feature.
///
/// When the `cli` feature is enabled, this is replaced by the full-featured
/// `adk_cli::Launcher` with `--serve` mode, readline history, and thinking
/// block rendering.
#[cfg(all(feature = "runner", not(feature = "cli")))]
#[cfg_attr(docsrs, doc(cfg(feature = "runner")))]
pub use adk_runner::Launcher;

/// Full-featured CLI launcher with console and serve modes.
///
/// Requires the `cli` feature, which no tier includes; add it to any preset.
/// Provides `--serve` mode, `rustyline` history, and `clap` CLI parsing.
#[cfg(feature = "cli")]
#[cfg_attr(docsrs, doc(cfg(feature = "cli")))]
pub use adk_cli::Launcher;

/// Real-time bidirectional streaming (voice, video).
///
/// Provides real-time audio/video streaming for voice-enabled agents:
/// - [`RealtimeAgent`](realtime::RealtimeAgent) - Agent with voice capabilities
/// - [`RealtimeRunner`](realtime::RealtimeRunner) - Session management and tool execution
/// - Multiple providers: OpenAI Realtime, Gemini Live
///
/// Available with feature: `realtime`
#[cfg(feature = "realtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "realtime")))]
pub mod realtime {
    pub use adk_realtime::*;
}

/// Browser automation (WebDriver).
///
/// Provides browser automation tools for agents:
/// - [`BrowserSession`](browser::BrowserSession) - WebDriver session management
/// - [`BrowserToolset`](browser::BrowserToolset) - Browser tools for agents
///
/// Available with feature: `browser`
#[cfg(feature = "browser")]
#[cfg_attr(docsrs, doc(cfg(feature = "browser")))]
pub mod browser {
    pub use adk_browser::*;
}

/// Agent evaluation framework.
///
/// Test and validate agent behavior:
/// - [`Evaluator`](eval::Evaluator) - Run evaluation suites
/// - [`EvaluationConfig`](eval::EvaluationConfig) - Configure evaluation parameters
///
/// Available with feature: `eval`
#[cfg(feature = "eval")]
#[cfg_attr(docsrs, doc(cfg(feature = "eval")))]
pub mod eval {
    pub use adk_eval::*;
}

/// Guardrails for safety and policy enforcement.
///
/// Validate agent inputs and outputs:
/// - [`GuardrailSet`](guardrail::GuardrailSet) - Collection of guardrails
/// - [`ContentFilter`](guardrail::ContentFilter) - Content safety filtering
///
/// Available with feature: `guardrail`
#[cfg(feature = "guardrail")]
#[cfg_attr(docsrs, doc(cfg(feature = "guardrail")))]
pub mod guardrail {
    pub use adk_guardrail::*;
}

/// Authentication and access control.
///
/// Manage agent permissions and identity:
/// - [`Permission`](auth::Permission) - Permission definitions
/// - [`AccessControl`](auth::AccessControl) - Access control enforcement
///
/// Available with feature: `auth`
#[cfg(feature = "auth")]
#[cfg_attr(docsrs, doc(cfg(feature = "auth")))]
pub mod auth {
    pub use adk_auth::*;
}

/// Agentic commerce and payment orchestration.
///
/// Provides protocol-neutral payment primitives and adapters for:
/// - ACP stable `2026-01-30`
/// - ACP experimental surfaces behind `acp-experimental`
/// - AP2 `v0.1-alpha` as of `2026-03-22`
///
/// Available with feature: `payments`
#[cfg(feature = "payments")]
#[cfg_attr(docsrs, doc(cfg(feature = "payments")))]
pub mod payment {
    pub use adk_payments::*;
}

/// Plugin system for extending agent behavior.
///
/// Extensible callback architecture for agent lifecycle hooks:
/// - Plugin registration and discovery
/// - Before/after hooks for agent operations
///
/// Available with feature: `plugin`
#[cfg(feature = "plugin")]
#[cfg_attr(docsrs, doc(cfg(feature = "plugin")))]
pub mod plugin {
    pub use adk_plugin::*;
}

/// Audio processing pipeline (experimental — `full` preset).
///
/// Provides audio capabilities for agents:
/// - [`TtsProvider`](audio::TtsProvider) - Text-to-speech synthesis
/// - [`SttProvider`](audio::SttProvider) - Speech-to-text transcription
/// - [`AudioProcessor`](audio::AudioProcessor) - Audio effects processing
/// - `AudioPipeline` - Composable audio pipelines
/// - Cloud providers: ElevenLabs, OpenAI, Gemini, Cartesia, Deepgram, AssemblyAI
/// - Local inference: MLX (Apple Silicon), ONNX Runtime
///
/// Available with feature: `audio`
#[cfg(feature = "audio")]
#[cfg_attr(docsrs, doc(cfg(feature = "audio")))]
pub mod audio {
    pub use adk_audio::*;
}

/// Retrieval-Augmented Generation (RAG) pipeline.
///
/// Modular RAG system with trait-based components:
/// - [`RagPipeline`](rag::RagPipeline) - Orchestrates ingest and query workflows
/// - [`RagTool`](rag::RagTool) - Agentic retrieval via `Tool` trait
/// - [`InMemoryVectorStore`](rag::InMemoryVectorStore) - Zero-dependency vector store
/// - Chunking strategies: fixed-size, recursive, markdown-aware
/// - Feature-gated backends: Gemini, OpenAI, Qdrant, LanceDB, pgvector
///
/// Available with feature: `rag`
#[cfg(feature = "rag")]
#[cfg_attr(docsrs, doc(cfg(feature = "rag")))]
pub mod rag {
    pub use adk_rag::*;
}

/// Shared action node types for graph workflows.
///
/// Provides the type definitions for all 14 action node types:
/// - Trigger nodes (manual, webhook, schedule, event)
/// - Data nodes (HTTP, Set, Transform)
/// - Control flow nodes (Switch, Loop, Merge, Wait)
/// - Compute nodes (Code)
/// - Infrastructure nodes (Database)
/// - Communication nodes (Email, Notification, RSS, File)
///
/// Available with feature: `action`
#[cfg(feature = "action")]
#[cfg_attr(docsrs, doc(cfg(feature = "action")))]
pub use adk_action;

/// Anthropic API client types and HTTP client.
///
/// Direct access to the `adk-anthropic` crate for low-level Anthropic API usage:
/// - [`Anthropic`](anthropic_client::Anthropic) - HTTP client struct
/// - Wire types: `MessageCreateParams`, `Message`, `ContentBlock`, etc.
/// - Streaming: `MessageStreamEvent`, `ContentBlockDelta`
/// - Error handling: `Error` enum with typed variants
///
/// For high-level agent usage, prefer `adk-model`'s `AnthropicClient` instead.
///
/// Available with feature: `anthropic-client`
#[cfg(feature = "anthropic-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "anthropic-client")))]
pub mod anthropic_client {
    pub use adk_anthropic::*;
}

// ============================================================================
// v0.7.0 Competitive Parity Feature Re-exports (opt-in only)
// ============================================================================

// --- adk-server features ---

/// YAML agent configuration loader and hot reload watcher.
///
/// Declaratively define agents in YAML files with hot reload support:
/// - [`YamlAgentDefinition`](server::yaml_agent::YamlAgentDefinition) - YAML schema types
/// - [`AgentConfigLoader`](server::yaml_agent::AgentConfigLoader) - Load and validate YAML agent files
/// - [`HotReloadWatcher`](server::yaml_agent::HotReloadWatcher) - Watch for file changes and reload agents
///
/// Available with feature: `yaml-agent`
#[cfg(feature = "yaml-agent")]
#[cfg_attr(docsrs, doc(cfg(feature = "yaml-agent")))]
pub mod yaml_agent {
    pub use adk_server::yaml_agent::*;
}

/// Agent Registry REST API for agent discovery and management.
///
/// Register, discover, and manage agents through a REST API:
/// - [`AgentCard`](server::registry::AgentCard) - Agent metadata
/// - [`AgentRegistryStore`](server::registry::AgentRegistryStore) - Storage backend trait
/// - [`InMemoryAgentRegistryStore`](server::registry::InMemoryAgentRegistryStore) - In-memory storage
/// - [`registry_router`](server::registry::registry_router) - Axum router for registry endpoints
///
/// Available with feature: `agent-registry`
#[cfg(feature = "agent-registry")]
#[cfg_attr(docsrs, doc(cfg(feature = "agent-registry")))]
pub mod registry {
    pub use adk_server::registry::*;
}

// --- adk-tool features ---

/// MCP sampling callback support.
///
/// Handle `sampling/createMessage` requests from MCP servers:
/// - [`SamplingHandler`](tool::sampling::SamplingHandler) - Trait for handling sampling requests
/// - [`LlmSamplingHandler`](tool::sampling::LlmSamplingHandler) - Default handler routing to agent's LLM
/// - [`SamplingRequest`](tool::sampling::SamplingRequest) / [`SamplingResponse`](tool::sampling::SamplingResponse) - Wire types
///
/// Available with feature: `mcp-sampling`
#[cfg(feature = "mcp-sampling")]
#[cfg_attr(docsrs, doc(cfg(feature = "mcp-sampling")))]
pub mod sampling {
    pub use adk_tool::sampling::*;
}

/// Native Slack toolset for agent-driven Slack interactions.
///
/// Built-in Slack tools for agents:
/// - [`SlackToolset`](tool::slack::SlackToolset) - Toolset implementing `adk_core::Toolset`
/// - Tools: `slack_send_message`, `slack_read_channel`, `slack_add_reaction`, `slack_list_threads`
///
/// Available with feature: `slack`
#[cfg(feature = "slack")]
#[cfg_attr(docsrs, doc(cfg(feature = "slack")))]
pub mod slack {
    pub use adk_tool::slack::*;
}

/// Native BigQuery toolset for data-analysis agents.
///
/// Built-in BigQuery tools for agents:
/// - [`BigQueryToolset`](tool::bigquery::BigQueryToolset) - Toolset implementing `adk_core::Toolset`
/// - Tools: `bigquery_execute_sql`, `bigquery_get_table_schema`, `bigquery_list_datasets`, `bigquery_list_tables`
///
/// Available with feature: `bigquery`
#[cfg(feature = "bigquery")]
#[cfg_attr(docsrs, doc(cfg(feature = "bigquery")))]
pub mod bigquery {
    pub use adk_tool::bigquery::*;
}

/// Native Spanner toolset for Cloud Spanner interactions.
///
/// Built-in Spanner tools for agents:
/// - [`SpannerToolset`](tool::spanner::SpannerToolset) - Toolset implementing `adk_core::Toolset`
/// - Tools: `spanner_execute_sql`, `spanner_get_table_schema`, `spanner_list_tables`
///
/// Available with feature: `spanner`
#[cfg(feature = "spanner")]
#[cfg_attr(docsrs, doc(cfg(feature = "spanner")))]
pub mod spanner {
    pub use adk_tool::spanner::*;
}

// --- adk-eval features ---

/// User personas for evaluation.
///
/// Define simulated user personas for realistic multi-turn test conversations:
/// - [`PersonaProfile`](eval::personas::PersonaProfile) - Persona definition
/// - [`UserSimulator`](eval::personas::UserSimulator) - Generate persona-driven messages
/// - [`PersonaRegistry`](eval::personas::PersonaRegistry) - Load personas from directory
///
/// Available with feature: `personas`
#[cfg(feature = "personas")]
#[cfg_attr(docsrs, doc(cfg(feature = "personas")))]
pub mod personas {
    pub use adk_eval::personas::*;
}

// --- adk-realtime features ---

/// Video avatar configuration for realtime sessions.
///
/// Attach a video avatar to realtime voice agents:
/// - [`AvatarConfig`](realtime::avatar::AvatarConfig) - Avatar source, lip-sync, and rendering settings
/// - [`LipSyncConfig`](realtime::avatar::LipSyncConfig) - Lip-sync configuration
/// - [`RenderingConfig`](realtime::avatar::RenderingConfig) - Rendering parameters
///
/// Available with feature: `video-avatar`
#[cfg(feature = "video-avatar")]
#[cfg_attr(docsrs, doc(cfg(feature = "video-avatar")))]
pub mod avatar {
    pub use adk_realtime::avatar::*;
}

// ============================================================================
// Managed Agent Runtime (feature-gated, experimental)
// ============================================================================

/// Managed agent runtime — durable, resumable, provider-neutral agent execution.
///
/// Provides the `ManagedAgentRuntime` trait and `DefaultManagedAgentRuntime`:
/// - [`ManagedAgentRuntime`](managed::ManagedAgentRuntime) - Central lifecycle trait
/// - [`DefaultManagedAgentRuntime`](managed::DefaultManagedAgentRuntime) - Default implementation
/// - [`ManagedAgentDef`](managed::types::ManagedAgentDef) - Declarative agent definition
/// - [`SessionEvent`](managed::types::SessionEvent) - Provider-neutral event stream
/// - [`UserEvent`](managed::types::UserEvent) - Client-to-agent events
/// - [`ModelResolver`](managed::ModelResolver) - ModelRef → Arc<dyn Llm> resolution
/// - [`ScriptedLlm`](managed::ScriptedLlm) - Deterministic testing double
///
/// STABILITY: Experimental, additive, feature-gated. No breaking changes to
/// existing `Runner`/`LlmAgent` APIs when this feature is disabled.
///
/// Available with feature: `managed-runtime`
#[cfg(feature = "managed-runtime")]
#[cfg_attr(docsrs, doc(cfg(feature = "managed-runtime")))]
pub mod managed {
    pub use adk_managed::*;
}

/// The governed, durable harness agent (spec 3.1 §8.4).
///
/// Everything `adk-harness` exports: [`HarnessAgent`](harness::HarnessAgent) and its
/// builder, the modes, standing approvals, the shell, todos, instructions, skills, team
/// members, plan graphs and goals. With the `cli` feature it also re-exports the CLI's
/// `HarnessLauncher`, which runs a harness at the console or behind the server,
/// `ConsolePrompter`, which asks for approvals at the terminal, and the `ConsoleIo` it
/// reads from.
///
/// Available with feature: `harness` (included in `full`)
///
/// # Example
///
/// ```rust
/// use adk_rust::harness::{HarnessAgent, HarnessMode, InstructionsConfig};
/// use adk_rust::model::{ScriptedLlm, ScriptedResponse};
/// use std::sync::Arc;
///
/// # fn main() -> adk_rust::Result<()> {
/// let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("done")]));
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(adk_devtools::Workspace::new(std::env::temp_dir()))
///     .project_instructions(InstructionsConfig { user_file: None, ..InstructionsConfig::default() })
///     .mode(HarnessMode::Plan)
///     .build()?;
/// assert_eq!(harness.info().default_mode, HarnessMode::Plan);
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "harness")]
#[cfg_attr(docsrs, doc(cfg(feature = "harness")))]
pub mod harness {
    #[cfg(feature = "cli")]
    pub use adk_cli::console::ConsoleIo;
    #[cfg(feature = "cli")]
    pub use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
    pub use adk_harness::*;
}

/// The agent kernel, the always-on form of the harness (spec 3.2 §8.2).
///
/// Everything `adk-kernel` exports: the [`Kernel`](kernel::Kernel) supervisor and its
/// builder, the process table, wake sources, the event bus and the
/// [`control`](kernel::control) plane `adk-rust up` serves.
///
/// Available with feature: `harness-kernel` (included in `full`)
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_rust::kernel::{Kernel, KernelConfig, QueueClass};
///
/// let kernel = Kernel::builder()
///     .config(KernelConfig::default().with_app_name("ops"))
///     .run_store(Arc::new(adk_rust::InMemoryRunStore::new()))
///     .build()
///     .unwrap();
/// assert_eq!(kernel.config().queues, QueueClass::defaults());
/// ```
#[cfg(feature = "harness-kernel")]
#[cfg_attr(docsrs, doc(cfg(feature = "harness-kernel")))]
pub mod kernel {
    pub use adk_kernel::*;
}

/// Agent surfaces: the engine every surface runs on, and the web home (spec 3.3).
///
/// Everything `adk-surface` exports: the [`SurfaceEngine`](surface::SurfaceEngine) with its
/// sources, decision cards in the words of spec 3.3 §8, the rules of what each surface may
/// decide, [`web::router`](surface::web::router) for `ServerBuilder::add_api_routes`, and
/// [`web::home`](surface::web::home), the page, for `ServerBuilder::add_root_routes`.
///
/// Available with feature: `surface` (opt-in, in no tier)
///
/// # Example
///
/// ```rust
/// use adk_rust::surface::{Caller, Origin, SurfaceKind, rules};
///
/// let chat = Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
/// assert!(!rules::may_resume(&chat));
/// ```
#[cfg(feature = "surface")]
#[cfg_attr(docsrs, doc(cfg(feature = "surface")))]
pub mod surface {
    pub use adk_surface::*;
}

/// The AWP 2.0 website product (spec 3.5 Part C, §7.1, D-14): site kinds and their charter
/// templates, the team of eight agents, onboarding, jobs, incidents, alarms and the weekly
/// review.
///
/// Available with feature: `website` (in `full`).
///
/// # Example
///
/// ```rust
/// use adk_rust::website::onboarding::{StartingPoint, plan};
///
/// // The onboarding plan for a site that starts from nothing.
/// let steps = plan(&StartingPoint::Nothing, None);
/// assert!(!steps.is_empty());
/// ```
#[cfg(feature = "website")]
#[cfg_attr(docsrs, doc(cfg(feature = "website")))]
pub mod website {
    pub use adk_website::*;
}

/// The site pipeline of an AWP 2.0 website (spec 3.5 §7.8): content as files, the built-in
/// generator, the launch gate, signed releases in the site log, hosts, migration and freeze.
///
/// Available with feature: `website` (in `full`).
///
/// # Example
///
/// ```rust
/// use adk_rust::site::content::SiteConfig;
///
/// let config = SiteConfig::from_toml("name = \"Example\"\ndomain = \"example.com\"\nlanguages = [\"en\"]\n").unwrap();
/// assert_eq!(config.base_url(), "https://example.com");
/// ```
#[cfg(feature = "website")]
#[cfg_attr(docsrs, doc(cfg(feature = "website")))]
pub mod site {
    pub use adk_site::*;
}

// ============================================================================
// Enterprise Client SDK (feature-gated, experimental)
// ============================================================================

/// Enterprise client SDK — native Rust client for the ADK-Rust Enterprise
/// Managed Agent Service.
///
/// Provides `EnterpriseClient` for interacting with the platform over HTTP/SSE:
/// - [`EnterpriseClient`](enterprise::EnterpriseClient) - Primary API client
/// - [`ClientConfig`](enterprise::ClientConfig) - Client configuration
/// - Agent, Environment, and Session CRUD
/// - SSE event streaming with auto-reconnect
/// - Vault and Memory management (beta)
///
/// This crate has zero dependency on `adk-model`, `adk-runner`, or `adk-agent` —
/// it communicates exclusively via HTTP with the managed agent platform.
///
/// STABILITY: Experimental, additive, feature-gated.
///
/// Available with feature: `enterprise-client`
#[cfg(feature = "enterprise-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "enterprise-client")))]
pub use adk_enterprise;

// ============================================================================
// Convenience Functions
// ============================================================================

/// Detect LLM provider from environment variables.
///
/// Checks environment variables in precedence order and returns the first
/// matching provider that was compiled through Cargo features. The default
/// `minimal` tier detects Gemini via `GOOGLE_API_KEY`; add `openai` or
/// `anthropic` to widen detection.
///
/// 1. `GOOGLE_GENAI_USE_ENTERPRISE` / `GOOGLE_GENAI_USE_VERTEXAI` truthy
///    (`1` or case-insensitive `true`) → Gemini on Vertex AI via Application
///    Default Credentials, using `GOOGLE_CLOUD_PROJECT` and
///    `GOOGLE_CLOUD_LOCATION` (requires the `gemini-vertex` feature;
///    `GOOGLE_GENAI_USE_ENTERPRISE` takes precedence when both are set)
/// 2. `ANTHROPIC_API_KEY` → Anthropic (Claude)
/// 3. `OPENAI_API_KEY` → OpenAI
/// 4. `GOOGLE_API_KEY` → Gemini
///
/// When a Vertex flag is truthy but the `gemini-vertex` feature is not
/// compiled, a `tracing` warning is emitted and detection falls through to
/// the API-key steps, which may select the Gemini Studio endpoint
/// (`generativelanguage.googleapis.com`).
///
/// # Errors
///
/// Returns [`AdkError`] when no supported environment variable is set, or
/// when a Vertex flag is truthy but `GOOGLE_CLOUD_PROJECT` /
/// `GOOGLE_CLOUD_LOCATION` is missing.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rust::provider_from_env;
/// use std::sync::Arc;
///
/// let model: Arc<dyn adk_rust::Llm> = provider_from_env()?;
/// ```
pub fn provider_from_env() -> Result<std::sync::Arc<dyn Llm>> {
    // The Vertex opt-in flags come before any API-key sniffing so a
    // deployment pinned to Vertex AI can never be diverted to the Studio
    // endpoint by a stray API key.
    #[cfg(feature = "gemini")]
    {
        if model::gemini::vertex_env_requested() {
            #[cfg(feature = "gemini-vertex")]
            {
                return Ok(std::sync::Arc::new(model::GeminiModel::from_env(
                    model::catalog::GEMINI_DEFAULT,
                )?));
            }
            #[cfg(not(feature = "gemini-vertex"))]
            tracing::warn!(
                env.flags = "GOOGLE_GENAI_USE_ENTERPRISE/GOOGLE_GENAI_USE_VERTEXAI",
                "vertex backend requested via environment but the gemini-vertex feature is not compiled; api-key detection may select the gemini studio endpoint (generativelanguage.googleapis.com)"
            );
        }
    }

    #[cfg(feature = "anthropic")]
    {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Ok(std::sync::Arc::new(model::anthropic::AnthropicClient::from_api_key(key)?));
        }
    }

    #[cfg(feature = "openai")]
    {
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            let config = model::openai::OpenAIConfig::new(key, model::catalog::OPENAI_DEFAULT);
            return Ok(std::sync::Arc::new(model::openai::OpenAIClient::new(config)?));
        }
    }

    #[cfg(feature = "gemini")]
    {
        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            return Ok(std::sync::Arc::new(model::GeminiModel::new(
                key,
                model::catalog::GEMINI_DEFAULT,
            )?));
        }
    }

    Err(AdkError::config(
        "No LLM provider detected. Set one of: ANTHROPIC_API_KEY, OPENAI_API_KEY, GOOGLE_API_KEY",
    ))
}

/// High-level single-turn agent invocation.
///
/// Creates an agent with the given instructions, sends the input, and returns
/// the text response. Uses [`provider_from_env`] to auto-detect the LLM provider.
///
/// This is the fastest way to get started with ADK — a single function call
/// that handles provider selection, session creation, agent building, and
/// execution.
///
/// # Arguments
///
/// * `instructions` - System instructions for the agent
/// * `input` - User input to send to the agent
///
/// # Returns
///
/// The agent's text response as a `String`.
///
/// # Errors
///
/// Returns [`AdkError`] when no supported environment variable is set, or
/// when agent execution fails.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rust::run;
///
/// let response = run("You are a helpful assistant.", "What is 2 + 2?").await?;
/// println!("{response}");
/// ```
#[cfg(all(feature = "agents", feature = "sessions", feature = "runner"))]
pub async fn run(instructions: &str, input: &str) -> Result<String> {
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::sync::Arc;

    type ProviderPair = (Arc<dyn Llm>, Option<Arc<dyn CacheCapable>>);

    let (model, cache_capable): ProviderPair = {
        #[allow(unused_assignments)]
        #[cfg_attr(
            not(any(feature = "anthropic", feature = "openai", feature = "gemini")),
            allow(unused_mut)
        )]
        let mut result: Option<ProviderPair> = None;

        #[cfg(feature = "anthropic")]
        {
            if result.is_none()
                && let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
            {
                let m = model::anthropic::AnthropicClient::from_api_key(key)?;
                result = Some((Arc::new(m), None));
            }
        }

        #[cfg(feature = "openai")]
        {
            if result.is_none()
                && let Ok(key) = std::env::var("OPENAI_API_KEY")
            {
                let config = model::openai::OpenAIConfig::new(key, model::catalog::OPENAI_DEFAULT);
                let m = model::openai::OpenAIClient::new(config)?;
                result = Some((Arc::new(m), None));
            }
        }

        #[cfg(feature = "gemini")]
        {
            if result.is_none()
                && let Ok(key) = std::env::var("GOOGLE_API_KEY")
            {
                let m = Arc::new(model::GeminiModel::new(key, model::catalog::GEMINI_DEFAULT)?);
                let cc: Arc<dyn CacheCapable> = m.clone();
                result = Some((m, Some(cc)));
            }
        }

        result.ok_or_else(|| {
            AdkError::config(
                "No LLM provider detected. Set one of: ANTHROPIC_API_KEY, OPENAI_API_KEY, GOOGLE_API_KEY",
            )
        })?
    };

    let agent =
        agent::LlmAgentBuilder::new("adk_run").instruction(instructions).model(model).build()?;

    let session_service: Arc<dyn adk_session::SessionService> =
        Arc::new(session::InMemorySessionService::new());

    let session_id = SessionId::generate();

    session_service
        .create(session::CreateRequest {
            app_name: "adk_run".into(),
            user_id: "user".into(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;

    let mut runner_builder = runner::Runner::builder()
        .app_name("adk_run")
        .agent(Arc::new(agent))
        .session_service(session_service);
    if let Some(cache_capable) = cache_capable {
        runner_builder = runner_builder.cache_capable(cache_capable);
    }
    let runner = runner_builder.build()?;

    let content = Content::new("user").with_text(input);
    let mut stream = runner.run(UserId::new("user")?, session_id, content).await?;

    let mut result = String::new();
    let mut text_deltas = adk_core::EventTextDeltas::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        let event = text_deltas.push(&event);
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Some(text) = part.text() {
                    result.push_str(text);
                }
            }
        }
    }

    Ok(result)
}

// ============================================================================
// Prelude
// ============================================================================

/// Convenience prelude for common imports.
///
/// Import everything you need with a single line:
///
/// ```
/// use adk_rust::prelude::*;
/// ```
///
/// This includes:
/// - Core traits: `Agent`, `Tool`, `Llm`, `Session`
/// - Agent builders: `LlmAgentBuilder`, `CustomAgentBuilder`
/// - Workflow agents: `SequentialAgent`, `ParallelAgent`, `LoopAgent`
/// - Models: `GeminiModel`
/// - Tools: `FunctionTool`, `GoogleSearchTool`, `McpToolset`
/// - Services: `InMemorySessionService`, `InMemoryArtifactService`
/// - Runtime: `Runner`, `RunnerConfig`
/// - Harness (feature `harness`): `HarnessAgent`, `HarnessAgentBuilder`, `HarnessMode`
/// - Common types: `Arc`, `Result`, `Content`, `Event`
pub mod prelude {
    // Core types (always available)
    pub use crate::{
        AdkError, Agent, BeforeModelResult, Content, Event, EventStream, InvocationContext, Llm,
        LlmRequest, LlmResponse, Part, Result, RunConfig, RunConfigBuilder, Session, State, Tool,
        ToolContext, Toolset,
    };

    // Agents
    #[cfg(feature = "agents")]
    pub use crate::agent::{
        ConditionalAgent, CustomAgent, CustomAgentBuilder, LlmAgent, LlmAgentBuilder,
        LlmConditionalAgent, LlmConditionalAgentBuilder, LoopAgent, ParallelAgent, SequentialAgent,
    };

    // Models
    #[cfg(feature = "gemini")]
    pub use crate::model::GeminiModel;

    // Model providers (when specific features are enabled)
    #[cfg(feature = "openai")]
    pub use crate::model::openai::{OpenAIClient, OpenAIConfig};

    #[cfg(feature = "openrouter")]
    pub use crate::model::openrouter::{
        OpenRouterApiMode, OpenRouterClient, OpenRouterConfig, OpenRouterPlugin,
        OpenRouterProviderPreferences, OpenRouterReasoningConfig, OpenRouterRequestOptions,
        OpenRouterResponseTool,
    };

    #[cfg(feature = "opencode")]
    pub use crate::model::opencode::{
        OpenCodeApi, OpenCodeClient, OpenCodeConfig, OpenCodeService,
    };

    #[cfg(feature = "anthropic")]
    pub use crate::model::anthropic::{AnthropicClient, AnthropicConfig, Effort, ThinkingMode};

    #[cfg(feature = "deepseek")]
    pub use crate::model::deepseek::{DeepSeekClient, DeepSeekConfig};

    #[cfg(feature = "groq")]
    pub use crate::model::groq::{GroqClient, GroqConfig};

    #[cfg(feature = "muse")]
    pub use crate::model::muse::{
        MuseAsrClient, MuseClient, MuseConfig, MuseFilesClient, MuseImagesClient,
        MuseResponsesClient,
    };

    #[cfg(feature = "ollama")]
    pub use crate::model::ollama::{OllamaConfig, OllamaModel};

    // OpenAI-compatible providers: use OpenAICompatible with provider presets
    // e.g. OpenAICompatibleConfig::fireworks(api_key, model)
    #[cfg(feature = "openai")]
    pub use crate::model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};

    #[cfg(feature = "bedrock")]
    pub use crate::model::bedrock::{BedrockClient, BedrockConfig};

    #[cfg(feature = "azure-ai")]
    pub use crate::model::azure_ai::{AzureAIClient, AzureAIConfig};

    // Tools
    #[cfg(feature = "mcp")]
    pub use crate::tool::McpToolset;
    #[cfg(feature = "tools")]
    pub use crate::tool::{
        BasicToolset, ExitLoopTool, FunctionTool, GoogleSearchTool, LoadArtifactsTool,
        UrlContextTool, WebSearchTool,
    };

    // Skills
    #[cfg(feature = "skills")]
    pub use crate::skill::{SelectionPolicy, SkillInjector, SkillInjectorConfig, load_skill_index};
    #[cfg(feature = "skills-progressive-disclosure")]
    pub use crate::skill::{SkillToolset, SkillToolsetConfig};

    // Sessions
    #[cfg(feature = "sessions")]
    pub use crate::session::InMemorySessionService;

    // Artifacts
    #[cfg(feature = "artifacts")]
    pub use crate::artifact::InMemoryArtifactService;

    // Memory
    #[cfg(feature = "memory")]
    pub use crate::memory::InMemoryMemoryService;

    // Runner
    #[cfg(feature = "runner")]
    pub use crate::runner::{Runner, RunnerConfig};

    // Graph workflows
    #[cfg(feature = "graph")]
    pub use crate::graph::{END, GraphAgent, NodeOutput, Router, START, StateGraph};

    // Harness
    #[cfg(feature = "harness")]
    pub use crate::harness::{HarnessAgent, HarnessAgentBuilder, HarnessMode};

    // Realtime
    #[cfg(feature = "realtime")]
    pub use crate::realtime::{
        RealtimeAgent, RealtimeAgentBuilder, RealtimeConfig, RealtimeModel, RealtimeRunner,
        RealtimeSession,
    };

    // Common re-exports
    pub use crate::anyhow::Result as AnyhowResult;
    pub use crate::async_trait;
    pub use std::sync::Arc;

    // Convenience functions
    pub use crate::provider_from_env;
    #[cfg(all(feature = "agents", feature = "sessions", feature = "runner"))]
    pub use crate::run;
}
