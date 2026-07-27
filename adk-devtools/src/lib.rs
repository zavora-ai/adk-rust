//! # adk-devtools
//!
//! Developer tools for ADK-Rust coding agents — `read_file`, `write_file`,
//! `edit_file`, `glob`, `grep`, and `bash` — all scoped to a sandboxed
//! [`Workspace`].
//!
//! These are the inner-loop tools a coding agent needs: read and edit files,
//! search the tree, and run commands/tests. Every operation is rooted at a
//! workspace directory and rejected if it escapes; mutations and `bash` are
//! gated by the workspace's capability flags.
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use adk_devtools::{DevToolset, Workspace};
//! use adk_agent::LlmAgentBuilder;
//! use std::sync::Arc;
//!
//! let workspace = Workspace::new("./my-repo");
//! let agent = LlmAgentBuilder::new("coding-agent")
//!     .model(model)
//!     .toolset(Arc::new(DevToolset::new(workspace)))
//!     .build()?;
//! ```
//!
//! ## Sandboxing
//!
//! [`Workspace`] enforces **path containment** (no escaping the root),
//! **read-only** mode (deny writes/bash), and a **bash timeout**. Phase 1 runs
//! `bash` host-local; for strong isolation, run behind a containerized
//! `CodeExecutor` (see `docs/design/coding-agent.md`). The policy vocabulary is
//! aligned with `adk-code`'s `SandboxPolicy` and will integrate with it directly
//! in a later phase.

mod error;
mod toolset;
mod workspace;

pub mod tools;

pub use error::DevToolError;
pub use tools::{BashTool, EditFileTool, GlobTool, GrepTool, ReadFileTool, WriteFileTool};
pub use toolset::DevToolset;
pub use workspace::{DEFAULT_ENV_ALLOWLIST, Workspace};
