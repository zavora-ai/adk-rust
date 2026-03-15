//! Code execution tools for ADK agents.
//!
//! This module provides language-preset tool wrappers over the `adk-code` execution
//! substrate. Each tool chooses a default backend and sandbox policy automatically.
//!
//! ## Available Tools
//!
//! - [`RustCodeTool`] — Primary Rust-first code execution tool using [`RustSandboxExecutor`].
//! - [`FrontendCodeTool`] — Placeholder frontend preset for collaborative workspace examples.
//! - [`JavaScriptCodeTool`] — Secondary scripting preset for lightweight transforms.
//! - [`PythonCodeTool`] — Container-backed Python execution preset.
//!
//! ## Scope Model
//!
//! Each tool declares the authorization scopes it requires via
//! [`Tool::required_scopes()`].  When a [`ScopeGuard`](adk_auth::ScopeGuard)
//! is active, the framework checks that the calling user possesses **all**
//! declared scopes before dispatching execution.
//!
//! | Tool | Required Scopes | Rationale |
//! |------|----------------|-----------|
//! | [`RustCodeTool`] | `code:execute`, `code:execute:rust` | Sandboxed Rust execution with strict defaults |
//! | [`JavaScriptCodeTool`] | `code:execute` | In-process embedded JS, no elevated access |
//! | [`PythonCodeTool`] | `code:execute`, `code:execute:container` | Container-backed, elevated mode |
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
//! ```rust,ignore
//! use adk_tool::{RustCodeTool, FrontendCodeTool, JavaScriptCodeTool, PythonCodeTool};
//! use std::sync::Arc;
//!
//! // Backend specialist
//! let backend_tool = Arc::new(RustCodeTool::backend());
//!
//! // Frontend specialist (placeholder until container backend ships)
//! let frontend_tool = Arc::new(FrontendCodeTool::react());
//!
//! // Lightweight JS transforms (placeholder until EmbeddedJsExecutor ships)
//! let js_tool = Arc::new(JavaScriptCodeTool::new());
//!
//! // Python execution (placeholder until ContainerCommandExecutor ships)
//! let py_tool = Arc::new(PythonCodeTool::new());
//! ```

mod frontend_code_tool;
mod javascript_code_tool;
mod python_code_tool;
mod rust_code_tool;

pub use frontend_code_tool::FrontendCodeTool;
pub use javascript_code_tool::JavaScriptCodeTool;
pub use python_code_tool::PythonCodeTool;
#[allow(deprecated)]
pub use rust_code_tool::RustCodeTool;

/// Re-export [`adk_code::CodeTool`] as the recommended replacement for
/// the deprecated [`RustCodeTool`].
#[cfg(feature = "code")]
pub use adk_code::CodeTool;
