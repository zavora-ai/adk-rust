//! # adk-acp — Agent Client Protocol integration for ADK-Rust
//!
//! Connect ADK-Rust applications to ACP-compatible coding agents, or expose an
//! ADK-Rust agent as the coding agent behind an ACP-compatible editor or client.
//! Both directions use stable ACP protocol version 1 through the official
//! `agent-client-protocol` Rust SDK.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_acp::AcpAgentTool;
//! use adk_agent::LlmAgentBuilder;
//! use std::sync::Arc;
//!
//! // Wrap any ACP-compatible coding agent as a tool your agent can delegate to.
//! let coding_agent = AcpAgentTool::new("my-coding-agent --acp")
//!     .description("Delegate repository work to an ACP coding agent");
//!
//! let agent = LlmAgentBuilder::new("orchestrator")
//!     .instruction("Use claude-code for complex refactoring tasks.")
//!     .model(model)
//!     .tool(Arc::new(coding_agent))
//!     .build()?;
//! ```
//!
//! ## What is ACP?
//!
//! The [Agent Client Protocol](https://agentclientprotocol.com/) standardizes
//! communication between code editors (IDEs, CLIs) and coding agents. It enables:
//!
//! - **Sessions**: Multi-turn coding conversations rooted in a project
//! - **Streaming**: Text, thoughts, tool progress, and completion updates
//! - **Host services**: Opt-in filesystem, terminal, and MCP capabilities
//! - **Permissions**: Typed choices that a host policy or user can approve
//! - **Lifecycle**: Create, resume, list, cancel, close, and delete sessions
//!
//! ## Choose a direction
//!
//! - Use [`AcpAgentTool`], [`AcpToolset`], [`AcpSession`], or [`stream_prompt`]
//!   when an ADK-Rust application should call an external coding agent.
//! - Enable the `server` feature and use `AcpServer` when an editor or ACP
//!   client should call an ADK-Rust agent.
//!
//! The client transport currently starts a local ACP subprocess over stdio.
//! The server also uses the official SDK's stdio transport. Project paths are
//! context, not an operating-system sandbox; applications must still enforce
//! their own filesystem and process boundaries.
//!
//! ## Features
//!
//! - **`default`**: Client and host APIs for connecting to ACP agents
//! - **`server`**: Server APIs for exposing ADK agents to ACP clients

#![warn(missing_docs)]

pub mod connection;
pub mod error;
pub mod host;
pub mod permissions;
pub mod session;
pub mod status;
pub mod streaming;
pub mod tool;
pub mod toolset;
pub mod usage;

/// ACP Server: expose ADK agents as ACP-compatible agents for IDE connections.
///
/// Enabled with the `server` feature flag. See [`server::AcpServer`] for usage.
#[cfg(feature = "server")]
pub mod server;

pub use connection::{AcpAgentConfig, prompt_agent, prompt_agent_with_policy};
pub use error::{AcpError, Result};
pub use host::{AcpFileSystem, AcpTerminal};
pub use permissions::{PermissionDecision, PermissionPolicy, PermissionRequest};
pub use session::{AcpCancellationHandle, AcpSession, PromptResult};
pub use status::{AgentStatus, StatusTracker};
pub use streaming::{OutputChunk, OutputStream, stream_prompt};
pub use tool::AcpAgentTool;
pub use toolset::AcpToolset;
pub use usage::{AcpUsage, AcpUsageStats, UsageTracker};

// Server re-exports (gated behind `server` feature)
#[cfg(feature = "server")]
pub use server::{AcpServer, AcpServerConfig, AcpServerConfigBuilder, AcpServerHandle};

// Re-export the SDK for advanced usage
pub use agent_client_protocol;
