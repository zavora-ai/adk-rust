//! # adk-acp — Agent Client Protocol integration for ADK-Rust
//!
//! Connect ADK-Rust applications to ACP-compatible coding agents, or expose an
//! ADK-Rust agent as the coding agent behind an ACP-compatible editor or client.
//! Both directions speak ACP protocol version 1 through the official
//! `agent-client-protocol` Rust SDK 5.2, re-exported as [`agent_client_protocol`].
//! The SDK's v1 connection guard negotiates the protocol version during
//! `initialize` on both sides.
//!
//! Beyond the stable v1 surface, adk-acp implements these unstable ACP
//! surfaces on both sides: session fork, archive, status, options and title,
//! authentication status, end-of-turn token usage, plan operations, subagent
//! updates, MCP over ACP and configurable LLM providers. Unstable surfaces may
//! change in any SDK minor release, so adk-acp depends on
//! `agent-client-protocol` `~5.2`.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_acp::AcpAgentTool;
//! use adk_core::Tool;
//! use std::sync::Arc;
//!
//! // Wrap any ACP-compatible coding agent as a tool your agent can delegate to,
//! // then register it with `LlmAgentBuilder::tool`.
//! let coding_agent: Arc<dyn Tool> = Arc::new(
//!     AcpAgentTool::new("my-coding-agent --acp")
//!         .name("coding_agent")
//!         .description("Delegate repository work to an ACP coding agent"),
//! );
//! # let _ = coding_agent;
//! ```
//!
//! ## What is ACP?
//!
//! The [Agent Client Protocol](https://agentclientprotocol.com/) standardizes
//! communication between code editors (IDEs, CLIs) and coding agents. It enables:
//!
//! - **Sessions**: Multi-turn coding conversations rooted in a project
//! - **Streaming**: Text, thoughts, tool progress, plans, notices, and completion updates
//! - **Host services**: Opt-in filesystem, terminal, and MCP capabilities,
//!   including ADK-Rust tools served over the ACP connection ([`AcpToolServer`])
//! - **Providers**: Clients can list and configure the agent's model providers
//! - **Permissions**: Typed choices that a host policy or user can approve
//! - **Elicitation**: Structured questions an agent asks its client
//! - **Authentication**: Agent-advertised methods, logout, and status
//! - **Lifecycle**: Create, load, resume, fork, list, archive, cancel, close, and delete sessions
//!
//! ## Choose a direction
//!
//! - Use [`AcpAgentTool`], [`AcpToolset`], [`AcpConnection`], [`AcpSession`],
//!   or [`stream_prompt`] when an ADK-Rust application should call an external
//!   coding agent.
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
//! - **`unstable-protocol-v2`**: Draft ACP protocol v2 on both sides, through
//!   the SDK's `unstable_protocol_v2`. The server serves v1 and v2 clients;
//!   [`AcpConnection::connect`] prefers v2 and falls back to v1. The SDK may
//!   change v2 in any 5.x minor release.

#![warn(missing_docs)]

pub mod client;
#[cfg(feature = "unstable-protocol-v2")]
mod client_v2;
pub mod connection;
pub mod content;
pub mod elicitation;
pub mod error;
pub mod host;
pub mod mcp;
pub mod permissions;
pub mod session;
pub mod status;
pub mod streaming;
pub mod tool;
pub mod toolset;
pub mod usage;

/// Adapters between the v1 vocabulary and draft protocol v2 (`unstable-protocol-v2`).
#[cfg(feature = "unstable-protocol-v2")]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) mod protocol_v2;
#[cfg(feature = "unstable-protocol-v2")]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) mod relay;

/// ACP Server: expose ADK agents as ACP-compatible agents for IDE connections.
///
/// Enabled with the `server` feature flag. See [`server::AcpServer`] for usage.
#[cfg(feature = "server")]
pub mod server;

pub use client::{AcpAgentInfo, AcpConnection, SUPPORTED_PROTOCOL_VERSIONS};
pub use connection::{
    AcpAgentConfig, prompt_agent, prompt_agent_content_with_policy, prompt_agent_with_policy,
};
pub use elicitation::{AcpElicitation, DeclineElicitation};
pub use error::{AcpError, Result};
pub use host::{AcpFileSystem, AcpTerminal};
pub use mcp::AcpToolServer;
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
