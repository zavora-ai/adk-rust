//! # adk-sandbox
//!
//! Isolated code execution runtime for ADK agents.
//!
//! This crate provides the [`SandboxBackend`] trait and two implementations:
//!
//! - **`ProcessBackend`** (default feature `process`): Executes code in child
//!   processes via `tokio::process::Command`. Enforces timeout and environment
//!   isolation but not memory or network isolation.
//!
//! - **`WasmBackend`** (feature `wasm`): Executes WebAssembly modules in-process
//!   via `wasmtime`. Enforces timeout, memory limits, and full sandboxing (no
//!   filesystem or network access).
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_sandbox::{ProcessBackend, ExecRequest, Language};
//! use std::time::Duration;
//! use std::collections::HashMap;
//!
//! let backend = ProcessBackend::default();
//! let request = ExecRequest {
//!     language: Language::Python,
//!     code: "print('hello')".to_string(),
//!     stdin: None,
//!     timeout: Duration::from_secs(30),
//!     memory_limit_mb: None,
//!     env: HashMap::new(),
//! };
//! let result = backend.execute(request).await?;
//! println!("stdout: {}", result.stdout);
//! ```
//!
//! ## Feature Flags
//!
//! | Feature   | Description                          | Default |
//! |-----------|--------------------------------------|---------|
//! | `process` | Subprocess execution via tokio       | ✅      |
//! | `wasm`    | In-process WASM execution via wasmtime | ❌    |

pub mod backend;
pub mod error;
pub mod sandbox;
pub mod tool;
pub mod types;

// Feature-gated modules
#[cfg(feature = "process")]
pub mod process;

#[cfg(feature = "wasm")]
pub mod wasm;

// Public re-exports
pub use backend::{BackendCapabilities, EnforcedLimits, SandboxBackend};
pub use error::SandboxError;
pub use sandbox::{
    AccessMode, AllowedPath, SandboxEnforcer, SandboxPolicy, SandboxPolicyBuilder, WrappedCommand,
    get_enforcer,
};
pub use tool::SandboxTool;
pub use types::{ExecRequest, ExecResult, Language};

#[cfg(feature = "process")]
pub use process::{ProcessBackend, ProcessConfig};

#[cfg(feature = "wasm")]
pub use wasm::WasmBackend;

// Platform-specific enforcer re-exports for sandbox-native convenience
#[cfg(all(feature = "sandbox-native", target_os = "macos"))]
pub use sandbox::macos::MacOsEnforcer;

#[cfg(all(feature = "sandbox-native", target_os = "linux"))]
pub use sandbox::linux::LinuxEnforcer;

#[cfg(all(feature = "sandbox-native", target_os = "windows"))]
pub use sandbox::windows::WindowsEnforcer;
