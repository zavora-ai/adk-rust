//! # adk-code
//!
//! First-class code execution substrate for ADK-Rust.
//!
//! This crate provides a typed executor abstraction, a truthful sandbox capability model,
//! and shared execution backends for agent tools, Studio code nodes, and generated projects.
//!
//! ## Overview
//!
//! `adk-code` owns:
//!
//! - Execution request and result types ([`ExecutionRequest`], [`ExecutionResult`])
//! - Backend interface ([`CodeExecutor`] trait)
//! - Sandbox policy model ([`SandboxPolicy`], [`BackendCapabilities`])
//! - Request validation helpers ([`validate_policy`], [`validate_request`])
//! - Workspace abstraction for collaborative project builds ([`Workspace`], [`CollaborationEvent`])
//! - Built-in execution backends (Rust sandbox, embedded JS, WASM guest, container)
//!
//! ## Product Direction
//!
//! The primary code-execution path is Rust-first:
//!
//! - Author Rust code
//! - Execute it live in a sandbox via [`CodeExecutor`]
//! - Export the same Rust body into generated projects
//!
//! Secondary support includes embedded JavaScript for lightweight transforms,
//! WASM guest modules for portable sandboxed plugins, and container-backed
//! execution for broader multi-language isolation.
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_code::{
//!     ExecutionLanguage, ExecutionPayload, ExecutionRequest,
//!     ExecutionResult, ExecutionStatus, SandboxPolicy,
//! };
//!
//! let request = ExecutionRequest {
//!     language: ExecutionLanguage::Rust,
//!     payload: ExecutionPayload::Source {
//!         code: r#"
//! fn run(input: serde_json::Value) -> serde_json::Value {
//!     serde_json::json!({ "greeting": "hello" })
//! }
//! "#.to_string(),
//!     },
//!     argv: vec![],
//!     stdin: None,
//!     input: None,
//!     sandbox: SandboxPolicy::strict_rust(),
//!     identity: None,
//! };
//!
//! assert_eq!(request.language, ExecutionLanguage::Rust);
//! ```
//!
//! ## Crate Relationships
//!
//! - Depends on [`adk-core`] for shared error types
//! - [`adk-tool`] depends on this crate for language-preset tool wrappers
//! - [`adk-studio`] depends on this crate for live runner and code generation

pub mod a2a_compat;
mod code_tool;
pub mod compat;
mod container;
pub mod diagnostics;
#[cfg(feature = "embedded-js")]
mod embedded_js;
mod error;
mod executor;
pub mod harness;
mod rust_executor;
mod rust_sandbox;
mod types;
mod wasm_guest;
mod workspace;

pub use code_tool::CodeTool;
#[cfg(feature = "docker")]
pub use container::DockerExecutor;
pub use container::*;
pub use diagnostics::{RustDiagnostic, parse_diagnostics};
#[cfg(feature = "embedded-js")]
pub use embedded_js::*;
pub use error::*;
pub use executor::*;
pub use harness::{HARNESS_TEMPLATE, validate_rust_source};
pub use rust_executor::{CodeResult, RustExecutor, RustExecutorConfig};
pub use rust_sandbox::*;
pub use types::*;
pub use wasm_guest::*;
pub use workspace::*;
