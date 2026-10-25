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
//! ```rust
//! use adk_devtools::{DevToolset, Workspace};
//! use std::sync::Arc;
//!
//! // `bash` is off by default; enable it only for agents that need a shell.
//! let workspace = Workspace::new("./my-repo").allow_bash(true);
//! assert!(workspace.bash_allowed());
//!
//! // Register with an agent: `LlmAgentBuilder::new("coding-agent").toolset(toolset)`.
//! let toolset = Arc::new(DevToolset::new(workspace));
//! # let _ = toolset;
//! ```
//!
//! ## Sandboxing
//!
//! [`Workspace`] enforces **path containment** (no escaping the root),
//! **read-only** mode (deny writes/bash), **opt-in bash** (off until
//! [`Workspace::allow_bash`] enables it), a **bash timeout** that a call
//! can shorten but not extend, and **secret exclusion**: `grep` and `glob` skip
//! files matching [`DEFAULT_EXCLUDE_GLOBS`] and `read_file` refuses them with
//! [`DevToolError::Excluded`]. `edit_file` requires a prior `read_file` by the same
//! session and agent.
//!
//! `bash` runs host-local through [`HostLauncher`] with a cleared environment plus
//! [`DEFAULT_ENV_ALLOWLIST`], so provider API keys in the agent process are not
//! visible to commands. The command reads end of file from standard input, runs in
//! a new session with no controlling terminal, and its process group is killed when
//! the shell exits, so nothing it starts outlives the call. For strong isolation,
//! give [`BashTool::with_launcher`] a [`CommandLauncher`] that wraps the script in a
//! sandbox, or run commands behind a containerized `adk-code` executor (see
//! `docs/design/coding-agent.md`). The policy vocabulary is aligned with
//! `adk-code`'s `SandboxPolicy`.

#[cfg(any(unix, windows))]
mod beneath;
mod error;
mod launcher;
mod toolset;
mod workspace;

pub mod tools;

/// A process a [`CommandLauncher`] started itself in a Job Object, which
/// [`CommandLauncher::spawn_contained`] returns (Windows, spec 3.2 §10 W5).
#[cfg(windows)]
pub use adk_sandbox::process_tree::ContainedChild;
pub use error::DevToolError;
pub use launcher::{CommandLauncher, HostLauncher};
pub use tools::{
    BashTool, BoundedOutput, DEFAULT_MAX_INLINE_BYTES, EditFileTool, GlobTool, GrepTool,
    INLINE_OUTPUT_CHARS, OutputFile, OutputStore, ReadFileTool, RunningCommand, WriteFileTool,
    bound_output, sniff_inline_mime,
};
pub use toolset::DevToolset;
pub use workspace::{
    DEFAULT_ENV_ALLOWLIST, DEFAULT_EXCLUDE_GLOBS, DEFAULT_MAX_OUTPUT_BYTES, Workspace,
};
