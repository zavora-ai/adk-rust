# adk-sandbox

Isolated code execution runtime for [ADK-Rust](https://github.com/zavora-ai/adk-rust) agents.

`adk-sandbox` provides the `SandboxBackend` trait and two implementations for executing code in isolation. It separates the *isolation concern* from language-specific toolchains — `adk-code` handles compilation and language pipelines, while `adk-sandbox` handles running the resulting code safely.

## Feature Flags

| Feature   | Description                                | Default | Extra Dependencies |
|-----------|--------------------------------------------|---------|--------------------|
| `process` | Subprocess execution via `tokio::process`  | ✅      | None (uses tokio)  |
| `wasm`    | In-process WASM execution via `wasmtime`   | ❌      | `wasmtime`, `wasmtime-wasi` |

## Backend Comparison

| Capability              | `ProcessBackend`         | `WasmBackend`            |
|-------------------------|--------------------------|--------------------------|
| Timeout enforcement     | ✅ `tokio::time::timeout` | ✅ Epoch-based interruption |
| Memory limit            | ❌ Not enforced           | ✅ `StoreLimitsBuilder`   |
| Network isolation       | ❌ Not enforced           | ✅ No WASI network        |
| Filesystem isolation    | ❌ Not enforced           | ✅ No WASI preopens       |
| Environment isolation   | ✅ `env_clear()` + explicit env | ✅ Full (no host access) |
| Output truncation       | ✅ 1 MB limit, UTF-8 safe | ✅ 1 MB capture pipes     |
| Supported languages     | Rust, Python, JS, TS, Command | Wasm only            |

`ProcessBackend` is honest about what it does *not* enforce. Use `WasmBackend` when you need full sandboxing with memory limits and no host access.

## Quick Start

```rust
use adk_sandbox::{ProcessBackend, ExecRequest, Language, SandboxBackend};
use std::time::Duration;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = ProcessBackend::default();

    let mut env = HashMap::new();
    env.insert("PATH".to_string(), std::env::var("PATH").unwrap_or_default());

    let request = ExecRequest {
        language: Language::Python,
        code: "print('hello from sandbox')".to_string(),
        stdin: None,
        timeout: Duration::from_secs(30),
        memory_limit_mb: None,
        env,
    };

    let result = backend.execute(request).await?;
    println!("stdout: {}", result.stdout);
    println!("exit_code: {}", result.exit_code);
    Ok(())
}
```

Note: `ExecRequest` has no `Default` implementation — `timeout` must always be set explicitly.

## SandboxTool (Agent Integration)

`SandboxTool` implements `adk_core::Tool`, making sandbox execution available to LLM agents. Errors are returned as structured JSON (never as `ToolError`), so the agent can reason about failures.

```rust
use adk_sandbox::{SandboxTool, ProcessBackend};
use std::sync::Arc;

let backend = Arc::new(ProcessBackend::default());
let tool = SandboxTool::new(backend);

// Use with any LLM agent
let agent = LlmAgentBuilder::new("sandbox_agent")
    .tool(Arc::new(tool))
    .build()?;
```

The tool accepts `language`, `code`, optional `stdin`, and optional `timeout_secs` parameters. It requires the `code:execute` scope.

## Error Handling

All backend errors use `SandboxError`:

| Variant            | When                                          |
|--------------------|-----------------------------------------------|
| `Timeout`          | Execution exceeded the configured timeout     |
| `MemoryExceeded`   | WASM module exceeded memory limit             |
| `ExecutionFailed`  | Internal error (I/O, spawn failure)           |
| `InvalidRequest`   | Unsupported language for this backend         |
| `BackendUnavailable` | Missing runtime or feature not enabled      |

Non-zero exit codes are **not** errors — they are returned in `ExecResult.exit_code`.

## License

Apache-2.0
