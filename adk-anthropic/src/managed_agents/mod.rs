//! Anthropic Managed Agents API client.
//!
//! This module provides a typed Rust client for the [Anthropic Managed Agents API][api],
//! a server-side agent runtime where Anthropic manages the agent loop, sandbox, and
//! tool execution. Sessions are long-running and stateful, communicating via
//! SSE-based event streaming.
//!
//! # Direct-Client Surface
//!
//! This is a **direct-client surface** — it is deliberately **not** wired into the
//! `adk-runner` `Agent` trait. Managed Agents sessions are long-running (minutes
//! to hours), stateful, and SSE-driven, which does not fit the synchronous per-turn
//! `generate_content` tool-loop contract that the runner expects. Use
//! [`ManagedAgentsClient`] directly instead of going through the runner.
//!
//! # API Version and Beta Header
//!
//! All requests include the following headers:
//! - `anthropic-version: 2023-06-01` — the Anthropic API version
//! - `anthropic-beta: managed-agents-2026-04-01` — the beta feature flag for managed agents
//!
//! The base URL targets `https://api.anthropic.com/v1/beta/` endpoints.
//!
//! # Capabilities
//!
//! - **Agent CRUD**: Create, list, get, and delete managed agent configurations
//! - **Environment CRUD**: Create, get, and delete sandbox environments (cloud or self-hosted)
//! - **Session CRUD**: Create, get, list, archive, and delete sessions
//! - **Event dispatch**: Send user events (messages, interrupts, tool results, confirmations, outcomes)
//! - **SSE streaming**: Receive real-time agent events (messages, tool use, status changes)
//! - **Custom tool flow**: Handle agent tool requests and return results
//! - **Tool confirmation**: Approve or deny built-in tool executions
//! - **Multiagent orchestration**: Configure agents that reference other agents as tools
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_anthropic::managed_agents::{
//!     ManagedAgentsClient, CreateAgentParams, CreateEnvironmentParams,
//!     CreateAgentParams, CreateEnvironmentParams, CreateSessionParams,
//!     ManagedAgentsClient, SessionEvent, ToolConfig, UserEvent,
//! };
//! use futures::StreamExt;
//!
//! // Create a client from an API key
//! let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
//!
//! // Create an agent
//! let agent = client.create_agent(CreateAgentParams {
//!     name: "My Agent".to_string(),
//!     model: serde_json::json!("claude-sonnet-4-6"),
//!     system: Some("You are a helpful assistant.".to_string()),
//!     description: None,
//!     tools: vec![ToolConfig::agent_toolset()],
//!     mcp_servers: vec![],
//!     skills: vec![],
//!     metadata: None,
//! }).await?;
//!
//! // Create an environment
//! let env = client.create_environment(
//!     CreateEnvironmentParams::cloud("my-env")
//! ).await?;
//!
//! // Create a session
//! let session = client.create_session(
//!     CreateSessionParams::new(&agent.id, &env.id)
//! ).await?;
//!
//! // Open stream first, then send message
//! let mut stream = client.stream_events(&session.id).await?;
//! client.send_event(&session.id, UserEvent::message("Hello!")).await?;
//!
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         SessionEvent::AgentMessage { .. } => println!("Got agent message"),
//!         SessionEvent::StatusIdle { .. } => break,
//!         _ => {}
//!     }
//! }
//! ```
//!
//! [api]: https://docs.anthropic.com/en/docs/agents-and-tools/managed-agents

mod client;
mod dreams;
mod events;
mod memory;
mod stream;
mod types;
mod vaults;
mod webhooks;

pub use client::*;
pub use dreams::*;
pub use events::*;
pub use memory::*;
pub use stream::*;
pub use types::*;
pub use vaults::*;
pub use webhooks::*;
