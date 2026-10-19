//! # adk-enterprise
//!
//! Native Rust SDK for the ADK-Rust Enterprise Managed Agent Service.
//!
//! This crate provides a typed, ergonomic interface for creating agents, managing sessions,
//! sending messages, streaming responses, and controlling the full lifecycle of managed agents
//! over HTTP/SSE.
//!
//! ## Design Goals
//!
//! - **Lightweight**: Zero dependency on `adk-model`, `adk-runner`, `adk-agent`, or any heavy
//!   runtime crate — only HTTP + SSE.
//! - **Wire-compatible**: Types serialize and deserialize to the service's wire shapes.
//! - **Ergonomic**: Convenience methods, builder patterns, typed events, minimal boilerplate.
//! - **Resilient**: Auto-reconnect SSE, retry on transient errors, configurable timeouts.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_enterprise::{EnterpriseClient, CreateAgentParams, SessionEvent};
//! use futures::StreamExt;
//!
//! let client = EnterpriseClient::from_env()?;
//!
//! let agent = client.create_agent(CreateAgentParams {
//!     name: "My Agent".into(),
//!     model: "gemini-3.7-flash".into(),
//!     ..Default::default()
//! }).await?;
//!
//! let session = client.create_session(&agent.id, None).await?;
//! let mut stream = client.stream_events(&session.id).await?;
//! client.send_message(&session.id, "Hello!").await?;
//!
//! while let Some(event) = stream.next().await {
//!     match event? {
//!         SessionEvent::Message { content, .. } => println!("{content:?}"),
//!         SessionEvent::StatusIdle { .. } => break,
//!         _ => {}
//!     }
//! }
//! ```

pub mod agents;
pub mod client;
mod client_environment;
mod client_events;
mod client_memory;
mod client_sessions;
mod client_vault;
pub mod config;
pub mod error;
pub mod idempotency;
pub(crate) mod response;
pub mod retry;
pub mod stream;
pub mod types;

pub use client::EnterpriseClient;
pub use config::ClientConfig;
pub use error::EnterpriseError;
pub use stream::EventStream;
pub use types::*;

/// Result type alias for the enterprise SDK.
pub type Result<T> = std::result::Result<T, EnterpriseError>;
