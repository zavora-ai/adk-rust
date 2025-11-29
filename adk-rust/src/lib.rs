//! # Agent Development Kit (ADK) for Rust
//!
//! A flexible and modular framework for developing and deploying AI agents.
//! While optimized for Gemini and the Google ecosystem, ADK is model-agnostic,
//! deployment-agnostic, and compatible with other frameworks.
//!
//! ## Quick Start
//!
//! ```no_run
//! use adk_rust::prelude::*;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     // Get API key
//!     let api_key = std::env::var("GOOGLE_API_KEY")?;
//!     
//!     // Create model
//!     let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
//!     
//!     // Build agent
//!     let agent = LlmAgentBuilder::new("assistant")
//!         .description("Helpful AI assistant")
//!         .model(Arc::new(model))
//!         .build()?;
//!     
//!     println!("Agent '{}' ready!", agent.name());
//!     Ok(())
//! }
//! ```
//!
//! ## Installation
//!
//! ```toml
//! [dependencies]
//! # Simple: Get everything
//! adk-rust = "0.1"
//!
//! # Minimal: Only agents + Gemini
//! adk-rust = { version = "0.1", default-features = false, features = ["minimal"] }
//!
//! # Custom: Pick what you need
//! adk-rust = { version = "0.1", default-features = false, features = ["agents", "gemini", "tools"] }
//! ```
//!
//! ## Features
//!
//! - **Agent Types**: LLM, Custom, Sequential, Parallel, Loop, Conditional
//! - **Models**: Gemini 2.0 Flash (extensible to other providers)
//! - **Tools**: Function tools, Google Search, MCP integration
//! - **State Management**: Sessions, Artifacts, Memory
//! - **Deployment**: CLI, REST API, A2A protocol
//!
//! ## Architecture
//!
//! ADK-Rust uses a layered architecture:
//!
//! - **Application Layer**: CLI, REST Server, Examples
//! - **Runner Layer**: Agent execution, context management
//! - **Agent Layer**: Agent implementations
//! - **Service Layer**: Models, Tools, Sessions, Storage
//!
//! ## Examples
//!
//! See the [examples directory](https://github.com/zavora-ai/adk-rust/tree/main/examples)
//! for 12 working examples demonstrating all features.

#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// ============================================================================
// Core (always available)
// ============================================================================

/// Core traits and types.
///
/// Always available regardless of feature flags.
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
/// Available with feature: `agents`
#[cfg(feature = "agents")]
#[cfg_attr(docsrs, doc(cfg(feature = "agents")))]
pub mod agent {
    pub use adk_agent::*;
}

/// Model integrations (Gemini, etc.).
///
/// Available with feature: `models`
#[cfg(feature = "models")]
#[cfg_attr(docsrs, doc(cfg(feature = "models")))]
pub mod model {
    pub use adk_model::*;
}

/// Tool system and built-in tools.
///
/// Available with feature: `tools`
#[cfg(feature = "tools")]
#[cfg_attr(docsrs, doc(cfg(feature = "tools")))]
pub mod tool {
    pub use adk_tool::*;
}

/// Session management.
///
/// Available with feature: `sessions`
#[cfg(feature = "sessions")]
#[cfg_attr(docsrs, doc(cfg(feature = "sessions")))]
pub mod session {
    pub use adk_session::*;
}

/// Artifact storage.
///
/// Available with feature: `artifacts`
#[cfg(feature = "artifacts")]
#[cfg_attr(docsrs, doc(cfg(feature = "artifacts")))]
pub mod artifact {
    pub use adk_artifact::*;
}

/// Memory system with semantic search.
///
/// Available with feature: `memory`
#[cfg(feature = "memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "memory")))]
pub mod memory {
    pub use adk_memory::*;
}

/// Agent execution runtime.
///
/// Available with feature: `runner`
#[cfg(feature = "runner")]
#[cfg_attr(docsrs, doc(cfg(feature = "runner")))]
pub mod runner {
    pub use adk_runner::*;
}

/// HTTP server (REST + A2A).
///
/// Available with feature: `server`
#[cfg(feature = "server")]
#[cfg_attr(docsrs, doc(cfg(feature = "server")))]
pub mod server {
    pub use adk_server::*;
}

/// Telemetry (OpenTelemetry integration).
///
/// Available with feature: `telemetry`
#[cfg(feature = "telemetry")]
#[cfg_attr(docsrs, doc(cfg(feature = "telemetry")))]
pub mod telemetry {
    pub use adk_telemetry::*;
}

/// CLI launcher for running agents.
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
/// # Example
///
/// ```no_run
/// use adk_rust::prelude::*;
/// ```
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
