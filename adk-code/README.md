# adk-code

Language-aware code execution toolchain for [ADK-Rust](https://github.com/zavora-ai/adk-rust).

[![Crates.io](https://img.shields.io/crates/v/adk-code.svg)](https://crates.io/crates/adk-code)
[![Documentation](https://docs.rs/adk-code/badge.svg)](https://docs.rs/adk-code)
[![License](https://img.shields.io/crates/l/adk-code.svg)](LICENSE)

## Overview

`adk-code` handles compilation, diagnostics, and language-specific pipelines. It delegates actual execution to `adk-sandbox` backends, cleanly separating language toolchains from isolation.

The crate provides:

- Typed executor abstraction (`CodeExecutor` trait) with lifecycle management
- Sandbox policy model (`SandboxPolicy`, `BackendCapabilities`) with fail-closed validation
- Rust-first code execution via `RustExecutor` (check → build → delegate) and legacy `RustSandboxExecutor`
- Embedded JavaScript execution via `EmbeddedJsExecutor` (boa_engine, `embedded-js` feature)
- WASM guest module execution via `WasmGuestExecutor` (phase 1 placeholder)
- Docker container execution via `DockerExecutor` (persistent, `docker` feature) and `ContainerCommandExecutor` (ephemeral, always available)
- `CodeTool` implementing `adk_core::Tool` for LLM agent integration
- Structured Rust compiler diagnostics parsing
- Workspace abstraction for multi-agent collaborative project builds
- A2A-compatible collaboration transport layer

## Architecture

```
Agent → CodeTool (adk-code)
            │
            ▼
       RustExecutor
       check → build → delegate
            │
            ▼
       SandboxBackend (adk-sandbox)
       ProcessBackend / WasmBackend
```

The `RustExecutor` pipeline:
1. Check — `rustc --error-format=json` → parse structured diagnostics → halt on errors
2. Build — compile to binary using the harness template
3. Execute — delegate to a `SandboxBackend` via `ExecRequest`

## Quick Start

```rust
use adk_code::{CodeTool, RustExecutor, RustExecutorConfig};
use adk_sandbox::ProcessBackend;
use std::sync::Arc;

let backend = Arc::new(ProcessBackend::default());
let executor = RustExecutor::new(backend, RustExecutorConfig::default());
let tool = CodeTool::new(executor);

// Use with any LLM agent
let agent = LlmAgentBuilder::new("code_agent")
    .instruction("Execute Rust code when asked to compute something.")
    .tool(Arc::new(tool))
    .build()?;
```

User code must provide `fn run(input: serde_json::Value) -> serde_json::Value`. The harness wraps it with `fn main()`, stdin parsing, and stdout serialization.

## Feature Flags

| Feature       | Description                              | Default |
|---------------|------------------------------------------|---------|
| (none)        | Core types, `RustExecutor`, `RustSandboxExecutor`, `ContainerCommandExecutor`, `WasmGuestExecutor`, `CodeTool`, `Workspace` | ✅ |
| `embedded-js` | `EmbeddedJsExecutor` via `boa_engine`    | ❌      |
| `docker`      | `DockerExecutor` via `bollard` (persistent Docker containers) | ❌ |

## Execution Backends

### Backend Matrix

| Backend | Isolation | Timeout | Network | Filesystem | Environment | Persistent |
|---------|-----------|---------|---------|------------|-------------|------------|
| `RustSandboxExecutor` | HostLocal | ✅ | ❌ | ❌ | ❌ | ❌ |
| `RustExecutor` | Delegated | ✅ | Delegated | Delegated | Delegated | ❌ |
| `EmbeddedJsExecutor` | InProcess | ✅ | ✅* | ✅* | ✅* | ❌ |
| `WasmGuestExecutor` | InProcess | ✅ | ✅* | ✅* | ✅* | ❌ |
| `ContainerCommandExecutor` | ContainerEphemeral | ✅ | ✅ | ✅ | ✅ | ❌ |
| `DockerExecutor` | ContainerPersistent | ✅ | ✅ | ✅ | ✅ | ✅ |

*Enforcement by omission — the engine has no APIs for these operations.

### RustSandboxExecutor (legacy)

Host-local Rust compilation and execution. Compiles with `rustc`, runs the binary as a child process. Honest about capabilities: can enforce timeouts and output truncation, but not network/filesystem/environment restrictions.

```rust
use adk_code::{RustSandboxExecutor, RustSandboxConfig, CodeExecutor};

let executor = RustSandboxExecutor::new(RustSandboxConfig {
    rustc_path: "rustc".to_string(),
    rustc_flags: vec![],
    serde_json_path: None,
});
```

### RustExecutor (new)

Separates compilation from isolation by delegating execution to a `SandboxBackend`. The check → build → execute pipeline provides structured diagnostics.

```rust
use adk_code::{RustExecutor, RustExecutorConfig};
use adk_sandbox::ProcessBackend;
use std::sync::Arc;

let backend = Arc::new(ProcessBackend::default());
let executor = RustExecutor::new(backend, RustExecutorConfig {
    rustc_path: "rustc".to_string(),
    serde_json_path: None,
    rustc_flags: vec![],
});
```

`RustExecutor::execute()` returns a `CodeResult` with:
- `exec_result` — sandbox execution result (stdout, stderr, exit_code, duration)
- `diagnostics` — compiler warnings from the check step
- `output` — structured JSON extracted from the last stdout line
- `display_stdout` — everything before the structured output line

### EmbeddedJsExecutor (`embedded-js` feature)

In-process JavaScript execution via `boa_engine`. Useful for lightweight transforms and deterministic state shaping. No network/filesystem/environment APIs available (enforcement by omission).

```rust
use adk_code::{EmbeddedJsExecutor, CodeExecutor, ExecutionRequest,
    ExecutionLanguage, ExecutionPayload, SandboxPolicy};

let executor = EmbeddedJsExecutor::new();
let request = ExecutionRequest {
    language: ExecutionLanguage::JavaScript,
    payload: ExecutionPayload::Source {
        code: "return input.x + 1;".to_string(),
    },
    argv: vec![],
    stdin: None,
    input: Some(serde_json::json!({ "x": 41 })),
    sandbox: SandboxPolicy::strict_js(), // 5-second timeout
    identity: None,
};
```

User code is wrapped in an IIFE so `return` works. Input is injected as a global `input` variable. Return value is converted to JSON.

### WasmGuestExecutor

Executes precompiled `.wasm` guest modules. Phase 1 is a placeholder that validates module format (magic number, minimum size) but does not execute. Full runtime integration is deferred.

```rust
use adk_code::{WasmGuestExecutor, WasmGuestConfig};

let executor = WasmGuestExecutor::with_config(WasmGuestConfig {
    max_memory_bytes: 64 * 1024 * 1024, // 64 MB
    max_fuel: Some(1_000_000_000),       // 1B instructions
});
```

Accepts only `ExecutionPayload::GuestModule` with `GuestModuleFormat::Wasm`. Source payloads are rejected with a descriptive error pointing to `EmbeddedJsExecutor` or `ContainerCommandExecutor`.

### ContainerCommandExecutor (always available)

Shells out to `docker run` (or `podman`) for each execution. Each call spawns a new ephemeral container. Simpler but less efficient than `DockerExecutor`.

```rust
use adk_code::{ContainerCommandExecutor, ContainerConfig};

let executor = ContainerCommandExecutor::new(ContainerConfig {
    runtime: "docker".to_string(),
    default_image: "python:3.12-slim".to_string(),
    extra_flags: vec![],
    auto_remove: true,
});
```

Supports Python, JavaScript, and Command languages. Enforces network policy via `--network=none`, filesystem via bind mounts, and environment via `--env`.

### DockerExecutor (`docker` feature)

Persistent Docker container that survives across multiple `execute()` calls. Uses `bollard` to manage the container lifecycle via the Docker API.

```rust
use adk_code::{DockerExecutor, DockerConfig, CodeExecutor};

let executor = DockerExecutor::new(
    DockerConfig::python()
        .pip_install(&["numpy", "pandas"])
        .with_network()
)?;
executor.start().await?;

// Multiple executions reuse the same container
let result1 = executor.execute(request1).await?;
let result2 = executor.execute(request2).await?;

executor.cleanup().await?; // Prefer explicit cleanup over Drop
```

Presets: `DockerConfig::python()`, `DockerConfig::node()`, `DockerConfig::custom("image")`.

Builder methods: `setup_command()`, `pip_install()`, `npm_install()`, `with_network()`, `bind_mount()`, `env()`.

Lifecycle: `start()` → `execute()` (reusable) → `stop()` / `cleanup()`. Set `auto_start: true` (default) to start on first execute.

## CodeTool

`CodeTool` implements `adk_core::Tool` (name: `code_exec`) and dispatches to `RustExecutor`. Errors are returned as structured JSON, never as `ToolError`.

Parameters schema:
- `language` — `"rust"` (default, only supported value in phase 1)
- `code` — Rust source code (required)
- `input` — optional JSON input passed to `run()`
- `timeout_secs` — 1–300, default 30

Required scopes: `code:execute`, `code:execute:rust`

## Structured Diagnostics

Compile errors include parsed `RustDiagnostic` structs with level, message, spans, and error codes:

```json
{
  "status": "compile_error",
  "diagnostics": [
    {
      "level": "error",
      "message": "expected `;`",
      "code": "E0308",
      "spans": [{ "file_name": "main.rs", "line_start": 3, "column_start": 15 }]
    }
  ]
}
```

Use `parse_diagnostics(stderr)` to parse `rustc --error-format=json` output into `Vec<RustDiagnostic>`.

## Sandbox Policy Model

`SandboxPolicy` describes requested execution constraints. `BackendCapabilities` describes what a backend can enforce. `validate_policy()` and `validate_request()` implement fail-closed semantics — execution is rejected before user code runs if the backend cannot enforce a requested control.

Preset policies:
- `SandboxPolicy::strict_rust()` — no network, no filesystem, no env, 30s timeout, 1 MB limits
- `SandboxPolicy::host_local()` — network allowed (host-local can't restrict), 30s timeout
- `SandboxPolicy::strict_js()` — same as strict_rust but 5s timeout

## Harness

The harness template (`HARNESS_TEMPLATE`) wraps user code:
- Injects `use serde_json::Value;`
- Provides `fn main()` that reads JSON from stdin, calls `run()`, prints JSON to stdout
- Only `serde_json` is available as an external crate; full std library is available

Source validation (`validate_rust_source()`) rejects:
- `fn main` — harness provides it
- `#![...]` — crate-level attributes not supported

Comment stripping (`strip_comments()`) reduces false positives from patterns in comments.

## Workspace

`Workspace` is a shared project context for multi-agent collaborative code generation. Specialist agents coordinate through typed `CollaborationEvent`s with ownership, correlation, and wait/resume semantics.

```rust
use adk_code::{Workspace, CollaborationEventKind};
use std::time::Duration;

let ws = Workspace::new("./my-project")
    .project_name("my-project")
    .session_id("session-123")
    .build();

// Request work from another specialist
ws.request_work("corr-1", "api-routes", "frontend_engineer");

// Publish completed work
ws.publish_work("corr-1", "api-routes", "backend_engineer",
    serde_json::json!({ "routes": ["/api/users"] }));

// Wait for correlated response
let result = ws.wait_for_work("corr-1", Duration::from_secs(5)).await;
```

Convenience methods on `Workspace`:

| Method | Event Kind |
|--------|------------|
| `request_work()` | `NeedWork` |
| `claim_work()` | `WorkClaimed` |
| `publish_work()` | `WorkPublished` |
| `request_feedback()` | `FeedbackRequested` |
| `provide_feedback()` | `FeedbackProvided` |
| `signal_blocked()` | `Blocked` |
| `signal_completed()` | `Completed` |

Wait methods: `wait_for(correlation_id, timeout)`, `wait_for_work()`, `wait_for_feedback()`, `wait_for_kind(correlation_id, kind, timeout)`.

The `WorkspaceBuilder` supports `project_name()`, `session_id()`, `created_at()`, and `channel_capacity()` (default 256).

## A2A Collaboration Transport

The `a2a_compat` module provides a `CollaborationTransport` trait abstracting the event transport layer. Phase 1 uses `LocalTransport` (in-process broadcast channel). The event model maps cleanly onto the ADK A2A protocol for future remote specialist execution:

| Collaboration Concept | A2A Concept |
|---|---|
| `CollaborationEvent` | A2A `Message` or `TaskStatusUpdateEvent` |
| `correlation_id` | A2A `task_id` |
| `producer` / `consumer` | A2A agent card sender / receiver |
| `NeedWork` | `Submitted` |
| `WorkClaimed` | `Working` |
| `WorkPublished` | `Completed` + artifact |
| `FeedbackRequested` | `InputRequired` |
| `Completed` | `Completed` (final) |

## Error Types

Two error enums:

`ExecutionError` — legacy executor errors:

| Variant | Description |
|---------|-------------|
| `UnsupportedPolicy` | Backend cannot enforce a requested sandbox control |
| `UnsupportedLanguage` | Backend does not support the requested language |
| `CompileFailed` | Compilation failed |
| `Timeout` | Execution exceeded timeout |
| `ExecutionFailed` | Runtime failure |
| `Rejected` | Rejected before running (policy/scope check) |
| `InvalidRequest` | Malformed request |
| `InternalError` | Thread panic or unexpected failure |

`CodeError` — new pipeline errors with structured diagnostics:

| Variant | Description |
|---------|-------------|
| `CompileError` | Compilation errors with `Vec<RustDiagnostic>` and raw stderr |
| `DependencyNotFound` | Required dependency (e.g., `serde_json`) not found |
| `Sandbox` | Underlying `SandboxError` from the backend |
| `InvalidCode` | Source code invalid before compilation |

Both implement `From<...> for AdkError` with appropriate component/category/code mappings.

## Migration from Previous API

The previous `adk-code` API (`CodeExecutor`, `ExecutionRequest`, `RustSandboxExecutor`, etc.) is deprecated. See the `compat` module for the full migration table.

| Old Type (deprecated) | New Type | Crate |
|----------------------|----------|-------|
| `CodeExecutor` | `SandboxBackend` | `adk-sandbox` |
| `ExecutionRequest` | `ExecRequest` | `adk-sandbox` |
| `ExecutionResult` | `ExecResult` | `adk-sandbox` |
| `RustSandboxExecutor` | `RustExecutor` | `adk-code` |
| `RustSandboxConfig` | `RustExecutorConfig` | `adk-code` |
| `RustCodeTool` (adk-tool) | `CodeTool` | `adk-code` |

Key API changes:
- `SandboxBackend` has no lifecycle methods — just `execute(ExecRequest)`
- `ExecRequest` is flat: `language`, `code`, `stdin`, `timeout`, `memory_limit_mb`, `env`
- `RustExecutor::new(backend, config)` takes a `SandboxBackend` instead of embedding isolation
- Deprecated aliases compile with warnings for one release cycle (removed in v0.6.0)

## License

Apache-2.0
