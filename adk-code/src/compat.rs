//! Choosing between the two code-execution layers.
//!
//! Code execution has two layers. Both are supported; neither is deprecated.
//!
//! | Layer | Backend trait | Request and result | Built on it |
//! |-------|---------------|--------------------|-------------|
//! | `adk-sandbox` | [`SandboxBackend`](adk_sandbox::SandboxBackend) | [`ExecRequest`](adk_sandbox::ExecRequest), [`ExecResult`](adk_sandbox::ExecResult) | [`RustExecutor`](crate::RustExecutor), [`CodeTool`](crate::CodeTool), `adk_sandbox::SandboxTool` |
//! | `adk-code` | [`CodeExecutor`](crate::CodeExecutor) | [`ExecutionRequest`](crate::ExecutionRequest), [`ExecutionResult`](crate::ExecutionResult), with a [`SandboxPolicy`](crate::SandboxPolicy) | [`RustSandboxExecutor`](crate::RustSandboxExecutor), [`ContainerCommandExecutor`](crate::ContainerCommandExecutor), the embedded JavaScript and Monty executors, and `adk-tool`'s `PythonCodeTool` and `JavaScriptCodeTool` |
//!
//! ## Key API Differences
//!
//! ### `SandboxBackend` and `CodeExecutor`
//!
//! `CodeExecutor` has lifecycle methods (`start`, `stop`, `restart`, `is_running`), reports
//! [`BackendCapabilities`](crate::BackendCapabilities), and takes an `ExecutionRequest` whose
//! `SandboxPolicy` it validates against them. `SandboxBackend` is minimal:
//! `execute(ExecRequest) -> Result<ExecResult, SandboxError>`, with isolation coming from
//! the backend, such as a `ProcessBackend` under an OS enforcer.
//!
//! ### `ExecutionRequest` and `ExecRequest`
//!
//! `ExecutionRequest` has `payload` (an enum with `Source` and `GuestModule`), `sandbox`
//! (the policy), `identity`, and `argv`. `ExecRequest` is flat: `language`, `code`,
//! `stdin`, `timeout`, `memory_limit_mb`, `env`.
//!
//! ### `RustSandboxExecutor` and `RustExecutor`
//!
//! `RustSandboxExecutor` compiles and runs Rust as host processes. `RustExecutor` delegates
//! execution to a `SandboxBackend`, separating compilation from isolation; construct it
//! with `RustExecutor::new(backend, config)`.
//!
//! ## Removed
//!
//! `adk-tool`'s `RustCodeTool` was removed; [`CodeTool`](crate::CodeTool) replaces it.
