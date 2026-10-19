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
//! use std::sync::Arc;
//!
//! use adk_cli::Launcher;
//! use adk_core::Agent;
//!
//! async fn launch(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
//!     // Chat by default; `serve --port 8080` starts the web server on 127.0.0.1
//!     Launcher::new(agent).run().await
//! }
//! ```
//!
//! ## Modes
//!
//! - **Interactive**: rustyline REPL with history, streaming output, and think-block rendering
//! - **Server**: HTTP server with web UI (`serve --port 8080`), listening on `127.0.0.1` by default

pub mod console;
pub mod launcher;
pub mod serve;

#[cfg(feature = "optimize")]
pub mod optimize;

pub use launcher::Launcher;
