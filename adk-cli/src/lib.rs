//! # adk-cli
//!
//! Command-line launcher for ADK agents.
//!
//! ## Overview
//!
//! This crate provides command-line tools:
//!
//! - [`Launcher`] - Interactive REPL and server modes
//! - [`SingleAgentLoader`] - Simple agent loader
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_cli::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // let agent = create_your_agent()?;
//!     // Launcher::new(Arc::new(agent)).run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Modes
//!
//! - **Interactive**: REPL with history and colored output
//! - **Server**: HTTP server with web UI

pub mod console;
pub mod serve;
pub mod config;
pub mod launcher;

pub use launcher::{Launcher, SingleAgentLoader};
