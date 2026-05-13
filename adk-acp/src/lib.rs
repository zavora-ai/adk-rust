//! # adk-acp — Agent Client Protocol integration for ADK-Rust
//!
//! Connect ADK agents to external ACP agents (Claude Code, Codex, etc.) and
//! optionally expose ADK agents as ACP-compatible agents for IDE connections.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_acp::AcpAgentTool;
//! use adk_agent::LlmAgentBuilder;
//! use std::sync::Arc;
//!
//! // Wrap Claude Code as a tool your agent can delegate to
//! let claude = AcpAgentTool::new("claude-code")
//!     .description("Delegate complex coding tasks to Claude Code");
//!
//! let agent = LlmAgentBuilder::new("orchestrator")
//!     .instruction("Use claude-code for complex refactoring tasks.")
//!     .model(model)
//!     .tool(Arc::new(claude))
//!     .build()?;
//! ```
//!
//! ## What is ACP?
//!
//! The [Agent Client Protocol](https://agentclientprotocol.com/) standardizes
//! communication between code editors (IDEs, CLIs) and coding agents. It enables:
//!
//! - **Tool use**: Agents can request permission to use tools
//! - **Streaming responses**: Real-time content delivery
//! - **Session management**: Multi-turn conversations with context
//! - **Proxy chains**: Middleware that intercepts/transforms messages
//!
//! ## Features
//!
//! - **`default`**: Client-side only (connect to ACP agents)
//! - **`server`**: Expose ADK agents as ACP-compatible agents (Phase 2)

#![warn(missing_docs)]

pub mod connection;
pub mod error;
pub mod tool;
pub mod toolset;

pub use connection::{AcpAgentConfig, prompt_agent};
pub use error::{AcpError, Result};
pub use tool::AcpAgentTool;
pub use toolset::AcpToolset;

// Re-export the SDK for advanced usage
pub use agent_client_protocol;
