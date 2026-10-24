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
//! - [`ConsoleConfirmationHandler`] — answers tool calls held for confirmation at the console
//! - [`ConsoleIo`] — the single terminal reader the REPL and every prompt share
//! - [`ConsoleCommand`] — slash commands registered with the REPL
//! - [`console::run_console`] — quick one-call REPL for examples
//! - [`serve::run_serve`] — quick one-call HTTP server for examples
//! - `harness` (feature `harness`, default) — the governed harness at the console and
//!   behind the server: `HarnessLauncher`, `ConsolePrompter`, `HarnessArgs`
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
//! - **Interactive**: rustyline REPL with history, streaming output, think-block rendering,
//!   and approval prompts for tool calls held for confirmation
//! - **Server**: HTTP server with the web home (`serve --port 8080`, feature `surface`), listening
//!   on `127.0.0.1` by default

pub mod console;
#[cfg(feature = "harness")]
pub mod harness;
pub mod launcher;
pub mod serve;
#[cfg(all(feature = "surface", feature = "harness"))]
mod surface;

#[cfg(feature = "optimize")]
pub mod optimize;

pub use console::{
    ConsoleCommand, ConsoleCommandContext, ConsoleConfirmationHandler, ConsoleInput, ConsoleIo,
};
pub use launcher::Launcher;
