//! Migration compatibility guide.
//!
//! This module documents the migration path from the legacy `adk-code` types
//! to the new `adk-sandbox` + redesigned `adk-code` types.
//!
//! ## Migration Guide
//!
//! | Old Type (deprecated) | New Type | Crate |
//! |----------------------|----------|-------|
//! | [`CodeExecutor`](crate::CodeExecutor) | [`SandboxBackend`](adk_sandbox::SandboxBackend) | `adk-sandbox` |
//! | [`ExecutionRequest`](crate::ExecutionRequest) | [`ExecRequest`](adk_sandbox::ExecRequest) | `adk-sandbox` |
//! | [`ExecutionResult`](crate::ExecutionResult) | [`ExecResult`](adk_sandbox::ExecResult) | `adk-sandbox` |
//! | [`RustSandboxExecutor`](crate::RustSandboxExecutor) | [`RustExecutor`](crate::RustExecutor) | `adk-code` |
//! | [`RustSandboxConfig`](crate::RustSandboxConfig) | [`RustExecutorConfig`](crate::RustExecutorConfig) | `adk-code` |
//! | `RustCodeTool` (adk-tool) | [`CodeTool`](crate::CodeTool) | `adk-code` |
//!
//! ## Key API Differences
//!
//! ### `CodeExecutor` → `SandboxBackend`
//!
//! The old `CodeExecutor` trait had lifecycle methods (`start`, `stop`, `restart`,
//! `is_running`) and a complex `ExecutionRequest` with `SandboxPolicy`. The new
//! `SandboxBackend` trait is minimal: `execute(ExecRequest) -> Result<ExecResult, SandboxError>`.
//!
//! ### `ExecutionRequest` → `ExecRequest`
//!
//! The old `ExecutionRequest` had `payload` (enum with `Source` and `GuestModule`),
//! `sandbox` (policy struct), `identity`, and `argv`. The new `ExecRequest` is flat:
//! `language`, `code`, `stdin`, `timeout`, `memory_limit_mb`, `env`.
//!
//! ### `RustSandboxExecutor` → `RustExecutor`
//!
//! The old executor embedded isolation logic. The new `RustExecutor` delegates
//! execution to a `SandboxBackend`, separating compilation from isolation.
//! Constructor changes from `RustSandboxExecutor::default()` to
//! `RustExecutor::new(backend, config)`.
//!
//! ## Timeline
//!
//! - **v0.5.0**: Old types deprecated with `#[deprecated]` attributes
//! - **v0.6.0**: Old types removed
