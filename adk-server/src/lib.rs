//! # adk-server
//!
//! HTTP server and A2A protocol for ADK agents.
//!
//! ## Overview
//!
//! This crate provides HTTP infrastructure:
//!
//! - [`create_app`] - Create the REST API server
//! - [`create_app_with_a2a`] - Add A2A discovery and JSON-RPC endpoints
//! - [`ServerBuilder`] - Custom routes, A2A, shutdown, governance, and feature-gated endpoints
//! - [`RemoteA2aAgent`] - Connect to remote A2A agents
//! - [`ServerConfig`] / [`SecurityConfig`] - Server configuration
//!
//! The server answers only loopback host names and IP addresses by default and allows no
//! cross-origin access; list public hosts with [`ServerConfig::with_allowed_hosts`] and browser
//! origins with [`ServerConfig::with_allowed_origins`].
//!
//! The `a2a-v1` feature adds the `a2a::v1` module: A2A v1.0.0 JSON-RPC and REST handlers for
//! all 11 operations, built on [`a2a-protocol-types`](https://crates.io/crates/a2a-protocol-types).
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{Agent, SingleAgentLoader};
//! use adk_server::{ServerConfig, create_app};
//! use adk_session::InMemorySessionService;
//!
//! async fn serve(agent: Arc<dyn Agent>) -> std::io::Result<()> {
//!     let config = ServerConfig::new(
//!         Arc::new(SingleAgentLoader::new(agent)),
//!         Arc::new(InMemorySessionService::new()),
//!     );
//!     let app = create_app(config);
//!     let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
//!     axum::serve(listener, app).await
//! }
//! ```
//!
//! ## A2A Protocol
//!
//! [`create_app_with_a2a`] and [`ServerBuilder::with_a2a`] expose an agent over A2A:
//!
//! - `GET /.well-known/agent-card.json` (and `/.well-known/agent.json`) - Agent card
//! - `POST /a2a` - JSON-RPC: `message/send`, `tasks/get`, `tasks/cancel`
//! - `POST /a2a/stream` - JSON-RPC streaming: `message/stream`, `message/send`
#![allow(clippy::result_large_err)]

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

#[cfg(any(feature = "openai-webhooks", feature = "webhook-ingress"))]
pub mod webhooks;

#[cfg(feature = "background")]
pub mod background;

#[cfg(feature = "approvals")]
pub mod approvals;

#[cfg(feature = "agent-engine")]
pub mod agent_engine;

#[cfg(feature = "agent-engine")]
pub use agent_engine::{
    AgentEngineOptions, AgentEngineState, agent_engine_router, build_agent_engine_app,
    serve_agent_engine,
};

// Background runs and cron scheduling re-exports
#[cfg(feature = "background")]
pub use background::{
    BackgroundState, DurableScheduler, DurableWorker, DurableWorkerConfig, RunStatus,
    RunStatusResponse, SchedulerTick, SubmitRunRequest, SubmitRunResponse, WorkerHandle,
    WorkflowExecutor, WorkflowRegistry, WorkflowRunContext, WorkflowState, background_runs_router,
    background_runs_router_with_state,
};
#[cfg(feature = "background")]
pub use background::{
    ConcurrencyPolicy, CreateCronJobRequest, CronJobResponse, CronJobStatus, CronState,
    cron_jobs_router, cron_jobs_router_with_state, start_cron_scheduler, validate_cron_expression,
};

pub use a2a::{
    A2aClient, Executor, ExecutorConfig, RemoteA2aAgent, RemoteA2aAgentBuilder, RemoteA2aConfig,
    agent_skills_from_index, build_agent_card, build_agent_skills,
};
#[cfg(feature = "a2a-v1")]
pub use a2a::{A2aServer, A2aServerApp, A2aServerBuilder};
pub use auth_bridge::{
    AuthenticatedCaller, RequestContext, RequestContextError, RequestContextExtractor,
};
pub use config::{SecurityConfig, ServerConfig};
pub use rest::{
    A2aController, A2aTaskRetention, RuntimeController, ServerBuilder, SessionController,
    ShutdownHandle, create_app, create_app_with_a2a, shutdown_signal,
};
