//! # adk-cli
#![allow(clippy::result_large_err)]
//!
//! Command-line launcher for ADK agents.
//!
//! ## Overview
//!
//! This crate provides:
//!
//! - [`Launcher`] — embeddable CLI that gives any agent a REPL and a web server
//! - [`console::run_console`] — quick one-call REPL for examples
//! - [`serve::run_serve`] — quick one-call HTTP server for examples
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_cli::Launcher;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> adk_core::Result<()> {
//!     // let agent = create_your_agent()?;
//!     // Launcher::new(Arc::new(agent)).run().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Modes
//!
//! - **Interactive**: rustyline REPL with history, streaming output, and think-block rendering
//! - **Server**: HTTP server with web UI (`serve --port 8080`)

pub mod console;
pub mod launcher;
pub mod serve;

#[cfg(feature = "optimize")]
pub mod optimize;

pub use launcher::Launcher;
