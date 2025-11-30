//! # adk-server
//!
//! HTTP server and A2A protocol for ADK agents.
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
//! - `GET /.well-known/agent.json` - Agent card
//! - `POST /a2a` - JSON-RPC endpoint
//! - `POST /a2a/stream` - SSE streaming

pub mod a2a;
pub mod config;
pub mod rest;
pub mod web_ui;

pub use a2a::{
    build_agent_card, build_agent_skills, A2aClient, Executor, ExecutorConfig, RemoteA2aAgent,
    RemoteA2aAgentBuilder, RemoteA2aConfig,
};
pub use config::ServerConfig;
pub use rest::{
    create_app, create_app_with_a2a, A2aController, RuntimeController, SessionController,
};
