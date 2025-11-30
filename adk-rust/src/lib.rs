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
//! ```no_run
//! use adk_rust::prelude::*;
//! use adk_rust::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let api_key = std::env::var("GOOGLE_API_KEY")?;
//!     let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
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
//! adk-rust = "0.1"
//! tokio = { version = "1.40", features = ["full"] }
//! dotenv = "0.15"  # For loading .env files
//! ```
//!
//! ### Feature Presets
//!
//! ```toml
//! # Full (default) - Everything included
//! adk-rust = "0.1"
//!
//! # Minimal - Agents + Gemini + Runner only
//! adk-rust = { version = "0.1", default-features = false, features = ["minimal"] }
//!
//! # Custom - Pick exactly what you need
//! adk-rust = { version = "0.1", default-features = false, features = [
//!     "agents", "gemini", "tools", "sessions"
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
//! let api_key = std::env::var("GOOGLE_API_KEY")?;
//! let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
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
//! let loop_agent = LoopAgent::new("iterative_refiner", refiner, 5);
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
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Deserialize, JsonSchema)]
//! struct WeatherInput {
//!     /// City name to get weather for
//!     city: String,
//! }
//!
//! #[derive(Debug, Serialize)]
//! struct WeatherOutput {
//!     temperature: f64,
//!     conditions: String,
//! }
//!
//! async fn get_weather(_ctx: ToolContext, input: WeatherInput) -> Result<WeatherOutput> {
//!     // Your weather API call here
//!     Ok(WeatherOutput {
//!         temperature: 72.0,
//!         conditions: "Sunny".to_string(),
//!     })
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
//! Connect to Model Context Protocol servers:
//!
//! ```no_run
//! use adk_rust::prelude::*;
//!
//! # async fn example() -> Result<()> {
//! // Connect to an MCP server (e.g., filesystem, database)
//! let mcp_tools = McpToolset::from_command("npx", &[
//!     "-y", "@anthropic/mcp-server-filesystem", "/path/to/dir"
//! ]).await?;
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
//!
//! # async fn example() -> Result<()> {
//! # let session_service: InMemorySessionService = todo!();
//! // Create a session
//! let session = session_service.create("user_123", None).await?;
//!
//! // Store state with scoped prefixes
//! let state = session.state();
//! state.set("app:config", "production");      // App-level config
//! state.set("user:preference", "dark_mode");  // User preferences
//! state.set("temp:cache", "computed_value");  // Temporary data
//!
//! // State persists across conversation turns
//! let config = state.get::<String>("app:config")?;
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
//!     // Log all agent invocations
//!     .before_agent(|ctx| {
//!         Box::pin(async move {
//!             println!("Agent starting: {}", ctx.agent_name);
//!             Ok(None) // Continue execution
//!         })
//!     })
//!     // Modify or cache model responses
//!     .after_model(|ctx, response| {
//!         Box::pin(async move {
//!             println!("Model responded with {} tokens", response.usage.output_tokens);
//!             Ok(response)
//!         })
//!     })
//!     // Track tool usage
//!     .before_tool(|ctx, name, args| {
//!         Box::pin(async move {
//!             println!("Calling tool: {} with {:?}", name, args);
//!             Ok(None)
//!         })
//!     })
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
//!
//! # async fn example() -> Result<()> {
//! # let artifact_service: InMemoryArtifactService = todo!();
//! // Save an artifact
//! let image_data = std::fs::read("chart.png")?;
//! artifact_service.save(
//!     "reports",           // namespace
//!     "sales_chart.png",   // filename
//!     &image_data,
//!     "image/png",         // MIME type
//! ).await?;
//!
//! // Load an artifact
//! let artifact = artifact_service.load("reports", "sales_chart.png", None).await?;
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
//! use adk_rust::server::{A2AServer, AgentCard};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! # let agent: std::sync::Arc<dyn adk_rust::Agent> = todo!();
//! # let session_service: std::sync::Arc<adk_rust::session::InMemorySessionService> = todo!();
//! # let artifact_service: std::sync::Arc<adk_rust::artifact::InMemoryArtifactService> = todo!();
//! let card = AgentCard::new("my_agent", "https://my-agent.example.com")
//!     .with_description("A helpful assistant")
//!     .with_skill("research", "Can search and summarize information");
//!
//! let server = A2AServer::new(agent, card, session_service, artifact_service);
//! server.serve(8080).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Observability
//!
//! Built-in OpenTelemetry support for production monitoring:
//!
//! ```no_run
//! use adk_rust::telemetry::{TelemetryConfig, init_telemetry};
//!
//! # fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let config = TelemetryConfig::new("my-agent-service")
//!     .with_otlp_endpoint("http://localhost:4317");
//!
//! init_telemetry(config)?;
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
//! | Feature | Description | Default |
//! |---------|-------------|---------|
//! | `agents` | Agent implementations | ✅ |
//! | `models` | Model integrations | ✅ |
//! | `gemini` | Gemini model support | ✅ |
//! | `tools` | Tool system | ✅ |
//! | `mcp` | MCP integration | ✅ |
//! | `sessions` | Session management | ✅ |
//! | `artifacts` | Artifact storage | ✅ |
//! | `memory` | Semantic memory | ✅ |
//! | `runner` | Execution runtime | ✅ |
//! | `server` | HTTP server | ✅ |
//! | `telemetry` | OpenTelemetry | ✅ |
//! | `cli` | CLI launcher | ✅ |
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
pub use tokio;
pub use async_trait::async_trait;
pub use futures;
pub use serde;
pub use serde_json;
pub use anyhow;

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

/// CLI launcher for running agents.
///
/// Quick way to run agents in console or server mode:
/// - [`Launcher`] - Main entry point for CLI apps
/// - [`SingleAgentLoader`] - Load a single agent
///
/// Available with feature: `cli`
#[cfg(feature = "cli")]
#[cfg_attr(docsrs, doc(cfg(feature = "cli")))]
pub use adk_cli::{Launcher, SingleAgentLoader};

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
        Agent, Content, Part, Event, EventStream,
        Llm, LlmRequest, LlmResponse,
        Tool, ToolContext, Toolset,
        Session, State,
        InvocationContext, RunConfig,
        AdkError, Result,
        BeforeModelResult,
    };

    // Agents
    #[cfg(feature = "agents")]
    pub use crate::agent::{
        LlmAgent, LlmAgentBuilder,
        CustomAgent, CustomAgentBuilder,
        SequentialAgent, ParallelAgent, LoopAgent, ConditionalAgent,
    };

    // Models
    #[cfg(feature = "models")]
    pub use crate::model::{
        GeminiModel,
    };

    // Tools
    #[cfg(feature = "tools")]
    pub use crate::tool::{
        FunctionTool,
        GoogleSearchTool,
        ExitLoopTool,
        LoadArtifactsTool,
        BasicToolset,
        McpToolset,
    };

    // Sessions
    #[cfg(feature = "sessions")]
    pub use crate::session::{
        InMemorySessionService,
    };

    // Artifacts
    #[cfg(feature = "artifacts")]
    pub use crate::artifact::{
        InMemoryArtifactService,
    };

    // Memory
    #[cfg(feature = "memory")]
    pub use crate::memory::{
        InMemoryMemoryService,
    };

    // Runner
    #[cfg(feature = "runner")]
    pub use crate::runner::{
        Runner,
        RunnerConfig,
    };

    // Common re-exports
    pub use std::sync::Arc;
    pub use crate::async_trait;
    pub use crate::anyhow::Result as AnyhowResult;
}
