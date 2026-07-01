//! A Python [`CodeRuntime`] for the ADK-Rust [`CodeActAgent`], backed by
//! [Pydantic Monty](https://github.com/pydantic/monty) — a minimal, secure,
//! Rust-native Python interpreter built for running LLM-generated code.
//!
//! [`MontyRuntime`] lets a `CodeActAgent` *act by writing Python*: the model emits a
//! script each turn, invokes your [`Tool`](adk_core::Tool)s with the built-in
//! `call_tool("name", {"arg": value})` function, composes their results with real
//! control flow, and returns a tagged value. Monty executes that script
//! in-process in microseconds, with no container and no subprocess — and can
//! snapshot a paused run to bytes, which is exactly what the CodeAct
//! suspend/resume model (HITL confirmation, long-running tools, durable
//! checkpoints) requires.
//!
//! # OS access
//!
//! Operating-system effects a script attempts — filesystem reads/writes,
//! `os.getenv`/`os.environ`, and `date.today()`/`datetime.now()` — are serviced
//! in-place by the runtime against a host-controlled [`OsAccess`] policy. They
//! are **not** tools: they never pause the agent loop. By default a runtime is
//! fully sandboxed (no filesystem access, empty environment), but you can grant
//! a script specific read-only or read-write paths and an explicit environment
//! map:
//!
//! ```no_run
//! use adk_codeact_monty::{MontyRuntime, PathAccess};
//!
//! let runtime = MontyRuntime::builder()
//!     .allow_path("/data", "/srv/agent/data", PathAccess::ReadOnly)
//!     .allow_path("/out", "/srv/agent/out", PathAccess::ReadWrite)
//!     .environ_var("PROJECT", "acme")
//!     .build();
//! # let _ = runtime;
//! ```
//!
//! Network and subprocess access have no Monty OS-call surface and remain
//! unavailable regardless of policy.
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use adk_agent::codeact::CodeActAgent;
//! use adk_codeact_monty::MontyRuntime;
//! # use adk_core::Llm;
//! # fn wire(model: Arc<dyn Llm>) -> Result<(), Box<dyn std::error::Error>> {
//! let agent = CodeActAgent::builder()
//!     .name("python_agent")
//!     .model(model)
//!     .runtime(Arc::new(MontyRuntime::new()))
//!     .instruction("Solve the task by writing Python.")
//!     // .tool(Arc::new(MyTool))
//!     .build()?;
//! # let _ = agent;
//! # Ok(())
//! # }
//! ```
//!
//! # Resource limits
//!
//! Cap a script's time, memory, or allocations with the builder — limits ride
//! along inside a serialized continuation, so a resumed run stays bounded:
//!
//! ```
//! use std::time::Duration;
//! use adk_codeact_monty::MontyRuntime;
//!
//! let runtime = MontyRuntime::builder()
//!     .max_duration(Duration::from_secs(2))
//!     .max_memory(64 * 1024 * 1024)
//!     .build();
//! # let _ = runtime;
//! ```
//!
//! [`CodeActAgent`]: adk_agent::codeact::CodeActAgent
//! [`CodeRuntime`]: adk_agent::codeact::CodeRuntime

#![warn(missing_docs)]

mod convert;
mod os_access;
mod prompt;
mod runtime;

pub use os_access::{OsAccess, OsAccessBuilder, PathAccess};
pub use runtime::{MontyRuntime, MontyRuntimeBuilder};

/// Re-export of Monty's resource-limit configuration, for
/// [`MontyRuntimeBuilder::resource_limits`].
pub use monty::ResourceLimits;
