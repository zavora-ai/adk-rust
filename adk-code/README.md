# adk-code

Language-aware code execution toolchain for [ADK-Rust](https://github.com/zavora-ai/adk-rust).

`adk-code` handles compilation, diagnostics, and language-specific pipelines. It delegates actual execution to `adk-sandbox` backends, cleanly separating language toolchains from isolation.

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
1. **Check** — `rustc --error-format=json` → parse structured diagnostics → halt on errors
2. **Build** — compile to binary using the harness template
3. **Execute** — delegate to a `SandboxBackend` via `ExecRequest`

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

## Backend Matrix

| Backend | Timeout | Memory | Network | Filesystem | Environment |
|---------|---------|--------|---------|------------|-------------|
| `ProcessBackend` | ✅ Enforced | ❌ Not enforced | ❌ Not enforced | ❌ Not enforced | ✅ Isolated |
| `WasmBackend` | ✅ Enforced | ✅ Enforced | ✅ Isolated | ✅ Isolated | ✅ Isolated |
| `EmbeddedJsExecutor` | ✅ Enforced | ❌ Not enforced | ✅ No API available | ✅ No API available | ✅ No API available |
| `DockerExecutor` | ✅ Enforced | ✅ Container limits | ✅ Configurable | ✅ Configurable | ✅ Configurable |

`EmbeddedJsExecutor` reports enforcement as `true` because `boa_engine` simply has no network/filesystem/environment APIs — isolation is by omission, not by active enforcement.

## Feature Flags

| Feature       | Description                              | Default |
|---------------|------------------------------------------|---------|
| (none)        | Core types, `RustExecutor`, `CodeTool`   | ✅      |
| `embedded-js` | `EmbeddedJsExecutor` via `boa_engine`    | ❌      |
| `docker`      | `DockerExecutor` via `bollard`           | ❌      |

## Tools

| Tool | Name | Language | Scopes |
|------|------|----------|--------|
| `CodeTool` | `code_exec` | Rust (phase 1) | `code:execute`, `code:execute:rust` |
| `SandboxTool` (adk-sandbox) | `sandbox_exec` | Multi-language | `code:execute` |

Both tools follow the error-as-information pattern: compile errors and runtime failures are returned as structured JSON, never as `ToolError`.

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
