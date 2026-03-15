# Code Execution

ADK-Rust provides code execution through two crates:

- **`adk-sandbox`** — Isolated execution runtime with `ProcessBackend` (subprocess) and `WasmBackend` (wasmtime)
- **`adk-code`** — Language-aware toolchain with `RustExecutor` (check, build, execute pipeline) and `CodeTool`

The separation is intentional: `adk-sandbox` handles isolation, `adk-code` handles compilation and language-specific logic.

## Quick Start

```rust
use adk_code::{CodeTool, RustExecutor, RustExecutorConfig};
use adk_sandbox::ProcessBackend;
use std::sync::Arc;

let backend = Arc::new(ProcessBackend::default());
let executor = RustExecutor::new(backend, RustExecutorConfig::default());
let tool = Arc::new(CodeTool::new(executor));

let agent = LlmAgentBuilder::new("code_agent")
    .instruction("Execute Rust code when asked to compute something.")
    .model(model)
    .tool(tool)
    .build()?;
```

For direct sandbox execution without the Rust compilation pipeline:

```rust
use adk_sandbox::{SandboxTool, ProcessBackend};
use std::sync::Arc;

let backend = Arc::new(ProcessBackend::default());
let tool = Arc::new(SandboxTool::new(backend));
```


## Architecture

`CodeTool` adds a Rust compilation pipeline (check for errors, build binary, then execute). `SandboxTool` executes code directly through the backend without compilation.

## Backend Capability Matrix

| Capability | `ProcessBackend` | `WasmBackend` | `EmbeddedJsExecutor` | `DockerExecutor` |
|------------|------------------|---------------|----------------------|------------------|
| Timeout | Enforced | Enforced | Enforced | Enforced |
| Memory limit | Not enforced | Enforced | Not enforced | Container limits |
| Network isolation | Not enforced | No WASI network | No API available | Configurable |
| Filesystem isolation | Not enforced | No WASI preopens | No API available | Configurable |
| Environment isolation | `env_clear()` | Full | No API available | Configurable |
| Languages | Rust, Python, JS, TS, Command | Wasm | JavaScript | Python, Node.js |

`ProcessBackend` is honest about what it does not enforce. Use `WasmBackend` for full sandboxing or `DockerExecutor` for container-level isolation.

## Tools

| Tool | Crate | Name | Language | Scopes |
|------|-------|------|----------|--------|
| `CodeTool` | `adk-code` | `code_exec` | Rust | `code:execute`, `code:execute:rust` |
| `SandboxTool` | `adk-sandbox` | `sandbox_exec` | Multi-language | `code:execute` |

Both tools follow the error-as-information pattern: errors are returned as structured JSON with a `"status"` field, never as `ToolError`. This lets agents reason about failures.

## Structured Diagnostics

`CodeTool` returns parsed compiler diagnostics for Rust compile errors:

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
  ],
  "stderr": "error: expected `;`"
}
```

Success responses include structured output extracted from the harness:

```json
{
  "status": "success",
  "stdout": "",
  "output": { "greeting": "hello" },
  "exit_code": 0,
  "duration_ms": 42
}
```

## Rust Code Contract

User code must provide `fn run(input: serde_json::Value) -> serde_json::Value`. The harness wraps it with `fn main()`, stdin parsing, and stdout serialization. User code must not define `fn main()` or use crate-level attributes (`#![...]`).

## Migration from Previous API

The previous `adk-code` types (`CodeExecutor`, `ExecutionRequest`, `RustSandboxExecutor`) and `adk-tool`'s `RustCodeTool` are deprecated. Use the new types instead:

| Deprecated | Replacement | Crate |
|------------|-------------|-------|
| `RustCodeTool` | `CodeTool` | `adk-code` |
| `RustSandboxExecutor` | `RustExecutor` | `adk-code` |
| `ExecutionRequest` | `ExecRequest` | `adk-sandbox` |
| `ExecutionResult` | `ExecResult` | `adk-sandbox` |
| `CodeExecutor` | `SandboxBackend` | `adk-sandbox` |

See `adk_code::compat` for the full migration guide. Deprecated aliases compile with warnings for one release cycle.

## Browser JavaScript

Browser-page JavaScript execution via `adk-browser` is a separate capability. It runs inside the remote page context and is not part of the code execution substrate. Use `adk-browser` tools for web automation and page interaction.

## Studio Integration

ADK Studio uses the same code execution pipeline:

- **Live runner**: Executes authored Rust through `RustExecutor` + `ProcessBackend`
- **Code generation**: Embeds the same Rust body into generated projects
- **Sandbox settings**: Map to backend-enforceable capabilities
