//! Code execution tools for ADK agents.
//!
//! This module provides language-preset tool wrappers over the `adk-code` execution
//! substrate. Each tool chooses a default backend and sandbox policy automatically.
//!
//! ## Available Tools
//!
//! - [`CodeTool`] — Recommended Rust code execution tool using `RustExecutor` + `SandboxBackend`.
//! - [`FrontendCodeTool`] — Placeholder frontend preset for collaborative workspace examples.
//! - [`JavaScriptCodeTool`] — Secondary scripting preset for lightweight transforms.
//! - [`PythonCodeTool`] — Container-backed CPython execution preset (full Python
//!   ecosystem: pip packages, C extensions, complete standard library).
//! - [`MontyPythonCodeTool`] — In-process Python execution via the Monty
//!   interpreter (one-shot or per-session REPL; no container required).
//!
//! ## Scope Model
//!
//! Each tool declares the authorization scopes it requires via
//! `Tool::required_scopes()`.  When a `ScopeGuard` (from `adk_auth`)
//! is active, the framework checks that the calling user possesses **all**
//! declared scopes before dispatching execution.
//!
//! | Tool | Required Scopes | Rationale |
//! |------|----------------|-----------|
//! | [`CodeTool`] | `code:execute`, `code:execute:rust` | Sandboxed Rust execution with strict defaults |
//! | [`JavaScriptCodeTool`] | `code:execute` | In-process embedded JS, no elevated access |
//! | [`PythonCodeTool`] | `code:execute`, `code:execute:container` | Container-backed, elevated mode |
//! | [`MontyPythonCodeTool`] | `code:execute` | In-process Monty interpreter, host-granted OS access only |
//! | [`FrontendCodeTool`] | `code:execute`, `code:execute:container` | Container-backed, elevated mode |
//!
//! ### Elevated Modes and Confirmation
//!
//! Certain execution modes go beyond the base scope and should be gated by
//! additional scopes and/or the ADK confirmation flow:
//!
//! - **Host execution** (`code:execute:host`): Runs on the local host without
//!   container isolation.  Confirmation is required unless explicitly disabled
//!   by the deployer.
//! - **Container execution** (`code:execute:container`): Spawns an isolated
//!   container.  Deployers should consider confirmation gating.
//! - **Network access** (`code:network`): Enables outbound network from the
//!   execution environment.  Confirmation is strongly recommended.
//! - **Writable filesystem** (`code:filesystem:write`): Grants write access
//!   beyond the default read-only sandbox.  Confirmation is strongly
//!   recommended.
//!
//! Generic command execution should **not** silently inherit the trust posture
//! of the Rust sandbox preset.
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_code::{RustExecutor, RustExecutorConfig};
//! use adk_sandbox::ProcessBackend;
//! use adk_tool::{
//!     CodeTool, FrontendCodeTool, JavaScriptCodeTool, MontyPythonCodeTool, PythonCodeTool,
//! };
//! use std::sync::Arc;
//!
//! // Rust code execution (recommended): compiles and runs through a sandbox backend
//! let backend = Arc::new(ProcessBackend::default());
//! let rust_tool = Arc::new(CodeTool::new(RustExecutor::new(backend, RustExecutorConfig::default())));
//!
//! // Frontend specialist (placeholder until container backend ships)
//! let frontend_tool = Arc::new(FrontendCodeTool::react());
//!
//! // Lightweight JS transforms (requires the `code-embedded-js` feature)
//! let js_tool = Arc::new(JavaScriptCodeTool::new());
//!
//! // Container-backed CPython execution
//! let py_tool = Arc::new(PythonCodeTool::new());
//!
//! // In-process Python execution (requires the `code-embedded-python` feature)
//! let monty_tool = Arc::new(MontyPythonCodeTool::new());
//! ```

mod frontend_code_tool;
mod javascript_code_tool;
mod monty_python_code_tool;
mod python_code_tool;

pub use frontend_code_tool::FrontendCodeTool;
pub use javascript_code_tool::JavaScriptCodeTool;
pub use monty_python_code_tool::MontyPythonCodeTool;
#[cfg(feature = "code-embedded-python")]
pub use monty_python_code_tool::MontyPythonCodeToolBuilder;
pub use python_code_tool::PythonCodeTool;

/// Re-export [`adk_code::CodeTool`] as the recommended code execution tool.
#[cfg(feature = "code")]
pub use adk_code::CodeTool;
