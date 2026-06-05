//! # adk-server
#![allow(clippy::result_large_err)]
//!
//! HTTP server and A2A v1.0.0 protocol for ADK agents.
//!
//! ## Overview
//!
//! This crate provides HTTP infrastructure:
//!
//! - [`create_app`] - Create REST API server
//! - [`create_app_with_a2a`] - Add A2A protocol support
//! - [`RemoteA2aAgent`] - Connect to remote A2A agents
//! - [`ServerConfig`] - Server configuration
//!
//! ## What's New in 0.6.0
//!
//! ### A2A v1.0.0 Protocol Compliance
//!
//! The `a2a::v1` module (behind the `a2a-v1` feature flag) implements the full A2A Protocol
//! v1.0.0 specification with all 11 JSON-RPC operations:
//!
//! - RFC 3339 timestamps on all task status changes
//! - Agent capabilities declaration via `build_v1_agent_card()`
//! - Message ID idempotency for `SendMessage`/`SendStreamingMessage`
//! - Push notification authentication (Bearer + `a2a-notification-token`)
//! - INPUT_REQUIRED multi-turn resume flow
//! - Input validation (parts, IDs, metadata size)
//! - `Content-Type: application/a2a+json` on JSON-RPC responses
//! - Task object as first SSE streaming event
//! - Context-scoped task lookup for multi-turn conversations
//! - Version negotiation via `A2A-Version` header
//!
//! Wire types powered by [`a2a-protocol-types`](https://crates.io/crates/a2a-protocol-types).
//!
//! ### Breaking Changes
//!
//! - `build_v1_agent_card()` now requires an `AgentCapabilities` parameter
//! - `TaskStore` trait gains `find_task_by_context()` method
//! - `PushNotificationSender` trait methods gain `config` parameter
//! - `message_stream()` and `tasks_subscribe()` return `StreamResponse` instead of `TaskStatusUpdateEvent`
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_server::{create_app, ServerConfig};
//! use std::sync::Arc;
//!
//! // let config = ServerConfig { ... };
//! // let app = create_app(config);
//! // let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
//! // axum::serve(listener, app).await?;
//! ```
//!
//! ## A2A Protocol
//!
//! Expose agents via Agent-to-Agent protocol:
//!
//! - `GET /.well-known/agent-card.json` - Agent card with capabilities
//! - `POST /jsonrpc` - JSON-RPC endpoint (all 11 v1 operations)
//! - REST routes for all operations

pub mod a2a;
pub mod auth_bridge;
pub mod config;
pub mod rest;
pub mod ui_protocol;
pub mod ui_types;
pub mod web_ui;

#[cfg(feature = "yaml-agent")]
pub mod yaml_agent;

#[cfg(feature = "agent-registry")]
pub mod registry;

#[cfg(feature = "openai-webhooks")]
pub mod webhooks;

#[cfg(feature = "background")]
pub mod background;

// Background runs and cron scheduling re-exports
#[cfg(feature = "background")]
pub use background::{
    BackgroundRun, BackgroundRunner, BackgroundState, RunStatus, RunStatusResponse, RunStore,
    SubmitRunRequest, SubmitRunResponse, WorkflowState, background_runs_router,
    background_runs_router_with_state,
};
#[cfg(feature = "background")]
pub use background::{
    ConcurrencyPolicy, CreateCronJobRequest, CronJob, CronJobResponse, CronJobStatus, CronState,
    cron_jobs_router, cron_jobs_router_with_state, start_cron_scheduler, validate_cron_expression,
};

pub use a2a::{
    A2aClient, Executor, ExecutorConfig, RemoteA2aAgent, RemoteA2aAgentBuilder, RemoteA2aConfig,
    build_agent_card, build_agent_skills,
};
#[cfg(feature = "a2a-v1")]
pub use a2a::{A2aServer, A2aServerApp, A2aServerBuilder};
pub use auth_bridge::{RequestContext, RequestContextError, RequestContextExtractor};
pub use config::{SecurityConfig, ServerConfig};
pub use rest::{
    A2aController, RuntimeController, ServerBuilder, SessionController, ShutdownHandle, create_app,
    create_app_with_a2a, shutdown_signal,
};
