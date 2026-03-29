//! # Agent Development Kit (ADK) for Rust
//!
//! [![Crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
//! [![Documentation](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
//! [![License](https://img.shields.io/crates/l/adk-rust.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)
//!
//! A flexible and modular framework for developing and deploying AI agents in Rust.
//! While optimized for Gemini and the Google ecosystem, ADK is model-agnostic,
//! deployment-agnostic, and compatible with other frameworks.
//!
//! ## Quick Start
//!
//! Create your first AI agent in minutes:
//!
//! ```ignore
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let api_key = std::env::var("GOOGLE_API_KEY")?;
//!     let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
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
//! adk-rust = "0.3"
//! tokio = { version = "1.40", features = ["full"] }
//! dotenvy = "0.15"  # For loading .env files
//! ```
//!
//! ### Feature Presets
//!
//! ```toml
//! # Standard (default) — agents, models, tools, sessions, runner, guardrails, auth
//! adk-rust = "0.4"
//!
//! # Full — standard + all stable specialist crates (graph, realtime, browser, eval, rag)
//! # Does NOT include experimental crates (code, sandbox, audio) — use `labs` for those
//! adk-rust = { version = "0.4", features = ["full"] }
//!
//! # Labs — standard + experimental crates (code, sandbox, audio)
//! adk-rust = { version = "0.4", features = ["labs"] }
//!
//! # Full + Labs — everything including experimental crates
//! adk-rust = { version = "0.4", features = ["full", "labs"] }
//!
//! # Minimal — just agents + Gemini + runner (fastest build)
//! adk-rust = { version = "0.4", default-features = false, features = ["minimal"] }
//!
//! # Custom — pick exactly what you need
//! adk-rust = { version = "0.4", default-features = false, features = [
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
//! The core agent type that uses Large Language Models for intelligent reasoning:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<()> {
//! let api_key = std::env::var("GOOGLE_API_KEY").map_err(|e| AdkError::config(e.to_string()))?;
//! let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
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
//! Give your agents capabilities beyond conversation:
//!
//! ### Function Tools - Custom Operations
//!
//! Convert any async function into a tool:
//!
//! ```no_run
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
//! # fn example() -> Result<()> {
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
//! - [`GoogleSearchTool`](tool::GoogleSearchTool) - Web search via Google
//! - [`ExitLoopTool`](tool::ExitLoopTool) - Control loop termination
//! - [`LoadArtifactsTool`](tool::LoadArtifactsTool) - Access stored artifacts
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
//!         .arg("@anthropic/mcp-server-filesystem")
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
//! Store and retrieve binary data (images, files, etc.):
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use adk_rust::artifact::{ArtifactService, SaveRequest, LoadRequest};
//!
//! # async fn example() -> Result<()> {
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
//! ```bash
//! # Run your agent as a web server
//! cargo run -- serve --port 8080
//! ```
//!
//! Provides endpoints:
//! - `POST /chat` - Send messages
//! - `GET /sessions` - List sessions
//! - `GET /health` - Health check
//!
//! ### Agent-to-Agent (A2A) Protocol
//!
//! Expose your agent for inter-agent communication:
//!
//! ```no_run
//! use adk_rust::server::{create_app_with_a2a, ServerConfig};
//! use adk_rust::AgentLoader;
//!
//! # async fn example() -> adk_rust::Result<()> {
//! # let agent_loader: std::sync::Arc<dyn AgentLoader> = todo!();
//! # let session_service: std::sync::Arc<dyn adk_rust::session::SessionService> = todo!();
//! // Create server with A2A protocol support
//! let config = ServerConfig::new(agent_loader, session_service);
//! let app = create_app_with_a2a(config, Some("http://localhost:8080"));
//!
//! // Run the server (requires axum dependency)
//! // let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
//! // axum::serve(listener, app).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Observability
//!
//! Built-in OpenTelemetry support for production monitoring:
//!
//! ```no_run
//! use adk_rust::telemetry::{init_telemetry, init_with_otlp};
//!
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Basic telemetry with console logging
//! init_telemetry("my-agent-service")?;
//!
//! // Or with OTLP export for distributed tracing
//! // init_with_otlp("my-agent-service", "http://localhost:4317")?;
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
//! | `agents` | Agent implementations | standard |
//! | `models` | Model integrations | standard |
//! | `gemini` | Gemini model support | standard |
//! | `tools` | Tool system | standard |
//! | `skills` | Skill discovery | standard |
//! | `sessions` | Session management | standard |
//! | `artifacts` | Artifact storage | standard |
//! | `memory` | Semantic memory | standard |
//! | `runner` | Execution runtime | standard |
//! | `telemetry` | OpenTelemetry | standard |
//! | `guardrail` | Input/output validation | standard |
//! | `auth` | Access control | standard |
//! | `plugin` | Plugin system | standard |
//! | `server` | HTTP server + A2A | standard |
//! | `cli` | CLI launcher | standard |
//! | `graph` | Graph workflows | full |
//! | `browser` | Browser automation | full |
//! | `eval` | Agent evaluation | full |
//! | `realtime` | Voice/audio streaming | full |
//! | `rag` | RAG pipeline | full |
//! | `code` | Code execution | labs (experimental) |
//! | `sandbox` | Sandboxed execution | labs (experimental) |
//! | `audio` | Audio processing | labs (experimental) |
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
/// - [`FunctionTool`](tool::FunctionTool) - Wrap async functions as tools
/// - [`GoogleSearchTool`](tool::GoogleSearchTool) - Web search
/// - [`ExitLoopTool`](tool::ExitLoopTool) - Control loop agents
/// - [`McpToolset`](tool::McpToolset) - MCP server integration
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
/// - Web UI integration
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

/// Code execution substrate (experimental — `labs` preset).
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

/// Isolated code execution runtime (experimental — `labs` preset).
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

/// CLI launcher for running agents.
///
/// Quick way to run agents in console or server mode:
/// - [`Launcher`] - Main entry point for CLI apps
///
/// Available with feature: `cli`
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

/// Audio processing pipeline (experimental — `labs` preset).
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
// Convenience Functions
// ============================================================================

/// Detect LLM provider from environment variables.
///
/// Checks environment variables in precedence order and returns the first
/// matching provider:
///
/// 1. `ANTHROPIC_API_KEY` → Anthropic (Claude)
/// 2. `OPENAI_API_KEY` → OpenAI
/// 3. `GOOGLE_API_KEY` → Gemini
///
/// # Errors
///
/// Returns [`AdkError`] when no supported environment variable is set.
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
    #[cfg(feature = "anthropic")]
    {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Ok(std::sync::Arc::new(model::anthropic::AnthropicClient::from_api_key(key)?));
        }
    }

    #[cfg(feature = "openai")]
    {
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            let config = model::openai::OpenAIConfig::new(key, "gpt-4o-mini");
            return Ok(std::sync::Arc::new(model::openai::OpenAIClient::new(config)?));
        }
    }

    #[cfg(feature = "gemini")]
    {
        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            return Ok(std::sync::Arc::new(model::GeminiModel::new(key, "gemini-2.5-flash")?));
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

    let (model, cache_capable): (Arc<dyn Llm>, Option<Arc<dyn CacheCapable>>) = {
        #[allow(unused_assignments)]
        let mut result: Option<(Arc<dyn Llm>, Option<Arc<dyn CacheCapable>>)> = None;

        #[cfg(feature = "anthropic")]
        {
            if result.is_none() {
                if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                    let m = model::anthropic::AnthropicClient::from_api_key(key)?;
                    result = Some((Arc::new(m), None));
                }
            }
        }

        #[cfg(feature = "openai")]
        {
            if result.is_none() {
                if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                    let config = model::openai::OpenAIConfig::new(key, "gpt-4o-mini");
                    let m = model::openai::OpenAIClient::new(config)?;
                    result = Some((Arc::new(m), None));
                }
            }
        }

        #[cfg(feature = "gemini")]
        {
            if result.is_none() {
                if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
                    let m = Arc::new(model::GeminiModel::new(key, "gemini-2.5-flash")?);
                    let cc: Arc<dyn CacheCapable> = m.clone();
                    result = Some((m, Some(cc)));
                }
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

    let runner = runner::Runner::new(runner::RunnerConfig {
        app_name: "adk_run".into(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable,
        request_context: None,
        cancellation_token: None,
    })?;

    let content = Content::new("user").with_text(input);
    let mut stream = runner.run(UserId::new("user")?, session_id, content).await?;

    let mut result = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;
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
/// - Common types: `Arc`, `Result`, `Content`, `Event`
pub mod prelude {
    // Core types (always available)
    pub use crate::{
        AdkError, Agent, BeforeModelResult, Content, Event, EventStream, InvocationContext, Llm,
        LlmRequest, LlmResponse, Part, Result, RunConfig, Session, State, Tool, ToolContext,
        Toolset,
    };

    // Agents
    #[cfg(feature = "agents")]
    pub use crate::agent::{
        ConditionalAgent, CustomAgent, CustomAgentBuilder, LlmAgent, LlmAgentBuilder,
        LlmConditionalAgent, LlmConditionalAgentBuilder, LoopAgent, ParallelAgent, SequentialAgent,
    };

    // Models
    #[cfg(feature = "models")]
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

    #[cfg(feature = "anthropic")]
    pub use crate::model::anthropic::{AnthropicClient, AnthropicConfig, Effort, ThinkingMode};

    #[cfg(feature = "deepseek")]
    pub use crate::model::deepseek::{DeepSeekClient, DeepSeekConfig};

    #[cfg(feature = "groq")]
    pub use crate::model::groq::{GroqClient, GroqConfig};

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
    #[cfg(feature = "tools")]
    pub use crate::tool::{
        BasicToolset, ExitLoopTool, FunctionTool, GoogleSearchTool, LoadArtifactsTool, McpToolset,
        UrlContextTool, WebSearchTool,
    };

    // Skills
    #[cfg(feature = "skills")]
    pub use crate::skill::{SelectionPolicy, SkillInjector, SkillInjectorConfig, load_skill_index};

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
