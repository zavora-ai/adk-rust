# Code Execution

ADK-Rust provides code execution through two crates:

- **`adk-sandbox`** — Isolated execution runtime with `ProcessBackend` (subprocess, optionally under an OS sandbox profile) and `WasmBackend` (wasmtime)
- **`adk-code`** — Language-aware toolchain with `RustExecutor` (check, build, execute pipeline) and `CodeTool`, plus container, embedded JavaScript, and Monty Python executors

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

## Install

| Need | Dependency |
|------|------------|
| `CodeTool`, `RustExecutor`, `ContainerCommandExecutor` | `adk-code = "3.0.0"` |
| `DockerExecutor` (persistent container through the Docker API) | `adk-code` with `docker` |
| `EmbeddedJsExecutor` / `JavaScriptCodeTool` live path | `adk-code` with `embedded-js` |
| Monty Python executors / `MontyPythonCodeTool` live path | `adk-code` with `embedded-python` |
| `ProcessBackend`, `SandboxTool` | `adk-sandbox = "3.0.0"` (`process` is a default feature) |
| `WasmBackend` | `adk-sandbox` with `wasm` |
| OS sandbox profiles | `adk-sandbox` with `sandbox-native` (see [Sandboxed Code Execution](../sandbox/index.md)) |
| `PythonCodeTool`, `JavaScriptCodeTool`, `MontyPythonCodeTool`, `FrontendCodeTool` | `adk-tool` with `code` (`code-embedded-js`, `code-embedded-python`, `code-docker` add the live paths) |

Through the umbrella crate, `adk-rust`'s `code-tools`, `code-embedded-js`,
`code-embedded-python`, and `code-docker` features enable the same features; `full`
includes `code`, `sandbox`, and `code-tools`.

## Architecture

`CodeTool` adds a Rust compilation pipeline (check for errors, build binary, then execute). `SandboxTool` executes code directly through the backend without compilation.

There are two execution layers, and both are current:

| Layer | Crate | Interface | Request and result | Used by |
|-------|-------|-----------|--------------------|---------|
| Sandbox backends | `adk-sandbox` | `SandboxBackend` | `ExecRequest`, `ExecResult` | `RustExecutor`, `CodeTool`, `SandboxTool` |
| Code executors | `adk-code` | `CodeExecutor` | `ExecutionRequest` (with a `SandboxPolicy`), `ExecutionResult` | `ContainerCommandExecutor`, `DockerExecutor`, `EmbeddedJsExecutor`, the Monty executors, `PythonCodeTool`, `JavaScriptCodeTool`, `FrontendCodeTool` |

## Backend Capability Matrix

| Capability | `ProcessBackend` | `ProcessBackend` + OS enforcer | `WasmBackend` | `EmbeddedJsExecutor` | Monty executors | `DockerExecutor` |
|------------|------------------|--------------------------------|---------------|----------------------|-----------------|------------------|
| Timeout | Enforced | Enforced | Enforced | Enforced at the deadline; runtime limits stop the interpreter | Enforced (in-VM) | Enforced |
| Memory limit | Not enforced | Not enforced | Enforced | Not enforced | Enforced | Container limits |
| Network isolation | Not enforced | Enforced unless the policy allows network | No WASI network | No API available | No API available | Configurable; stricter requests rejected |
| Filesystem isolation | Not enforced | Writes confined to the policy's paths; reads too on macOS and Linux | No WASI preopens | No API available | Granted mounts only | Configurable; stricter requests rejected |
| Environment isolation | `env_clear()` | `env_clear()` | Full | No API available | Granted map only | Configurable; stricter requests rejected |
| Languages | Rust, Python, JS, TS, Command | Same | Wasm | JavaScript | Python | Python, Node.js |

`ProcessBackend` is honest about what it does not enforce: `capabilities()` and
`isolation()` report what applies. Attach an OS enforcer with
`ProcessBackend::with_sandbox`, use `WasmBackend` for full sandboxing, or use
`DockerExecutor` for container-level isolation.

`RustExecutor` runs `rustc` from a cleared environment plus the toolchain allowlist (`ProcessBackend::toolchain_env()`), so `env!` and `option_env!` cannot embed host credentials. `rustc` runs inside the backend on Unix hosts when the backend reports `filesystem_read_isolation` and supports commands, and on the host otherwise. Code is compiled with `--edition 2021` against a prebuilt `serde_json`: set `RustExecutorConfig::serde_json_path`, or `RustExecutor` looks in the `debug/deps` directory of the target directory that `cargo metadata` reports for the current directory; without it `CodeTool` returns `"status": "error"` with `dependency not found: serde_json` and the places searched.

`DockerExecutor` fixes its network mode, bind mounts, and environment when the container is created. `DockerConfig::validate_sandbox_policy()` checks each request against them, and a request stricter than the container fails with `ExecutionError::UnsupportedPolicy`. `DockerConfig::python()` (`python:3.12-slim`), `DockerConfig::node()` (`node:20-slim`), and `DockerConfig::custom(image)` start with networking disabled.

Both container executors stop runaway code and limit what it can do:

| Behaviour | `ContainerCommandExecutor` | `DockerExecutor` |
|-----------|----------------------------|------------------|
| Timeout | Runs `{runtime} kill {name}` on the named container; also when the call is dropped | Kills the exec's process group (or process) inside the persistent container |
| Capabilities | `--cap-drop=ALL` (`ContainerConfig::drop_capabilities`) | `CapDrop: ["ALL"]` (`DockerConfig::drop_capabilities`) |
| Privilege escalation | `--security-opt=no-new-privileges` (`no_new_privileges`) | `SecurityOpt: ["no-new-privileges"]` (`no_new_privileges`) |
| Process limit | `--pids-limit=256` (`pids_limit`, `None` for none) | `PidsLimit: 256` (`pids_limit`) |
| Output limits | Read up to the request's byte limits | Discarded past the request's byte limits as it streams |

The three hardening fields default to `true`, `true`, and `Some(adk_code::DEFAULT_PIDS_LIMIT)` (256); a struct literal needs them or `..ContainerConfig::default()`. `ContainerConfig::default()` runs `docker` with `python:3.12-slim`.

> **Note:** Setup commands that switch users, such as `apt-get install`, fail without capabilities; set `drop_capabilities: false` for an image that needs them.

## Tools

| Tool | Crate | Name | Language | Scopes |
|------|-------|------|----------|--------|
| `CodeTool` | `adk-code` | `code_exec` | Rust | `code:execute`, `code:execute:rust` |
| `SandboxTool` | `adk-sandbox` | `sandbox_exec` | Multi-language | `code:execute` |
| `PythonCodeTool` | `adk-tool` | `python_code` | Python (container-backed CPython, `python:3.12-slim` by default) | `code:execute`, `code:execute:container` |
| `JavaScriptCodeTool` | `adk-tool` | `javascript_code` | JavaScript (in-process boa engine; needs `code-embedded-js`) | `code:execute` |
| `MontyPythonCodeTool` | `adk-tool` | `monty_python_code` | Python (in-process Monty) | `code:execute` |
| `FrontendCodeTool` | `adk-tool` | `frontend_code` | JavaScript in a `node:20-slim` container (`react()`, `new(framework)`) | `code:execute`, `code:execute:container` |

`CodeTool`, `SandboxTool`, and `MontyPythonCodeTool` follow the error-as-information pattern: errors are returned as structured JSON with a `"status"` field, never as `ToolError`. This lets agents reason about failures. `PythonCodeTool` and `FrontendCodeTool` return a `ToolError` for a missing `code` argument and for executor-level failures; script-level failures surface as data. With the default `ContainerCommandExecutor` a timeout is data (`"status": "timeout"`); with a `DockerExecutor` it is an executor failure, so the call fails.

`CodeTool` and `SandboxTool` write snake_case keys (`exit_code`, `duration_ms`); `PythonCodeTool`, `JavaScriptCodeTool`, and `FrontendCodeTool` write camelCase keys (`exitCode`, `durationMs`).

For in-process Python execution — one-shot and REPL modes, OS-access grants, and host functions — see [Python Code Execution](python-code-execution.md).

## Structured Diagnostics

`CodeTool` returns parsed compiler diagnostics for Rust compile errors. Line numbers refer
to the harnessed source file, which wraps your code:

```json
{
  "status": "compile_error",
  "diagnostics": [
    {
      "level": "error",
      "message": "mismatched types",
      "code": "E0308",
      "spans": [
        { "file_name": "/tmp/.tmpGxoo4k/main.rs", "line_start": 4, "line_end": 4, "column_start": 18, "column_end": 24 }
      ]
    },
    { "level": "error", "message": "aborting due to 1 previous error", "code": null, "spans": [] }
  ],
  "stderr": "<rustc's JSON diagnostic stream>"
}
```

Success responses include structured output extracted from the harness, the program's
other stdout, and any compiler warnings:

```json
{
  "status": "success",
  "stdout": "log line",
  "stderr": "",
  "output": { "greeting": "hello" },
  "exit_code": 0,
  "duration_ms": 459,
  "diagnostics": [
    { "level": "warning", "message": "unused variable: `input`", "code": "unused_variables", "spans": [ ... ] }
  ]
}
```

`timeout_secs` (default 30) is clamped to 1–300 seconds; a run past it returns
`"status": "timeout"`.

## Rust Code Contract

User code must provide `fn run(input: serde_json::Value) -> serde_json::Value`. The harness wraps it with `fn main()`, stdin parsing, and stdout serialization. User code must not define `fn main()` or use crate-level attributes (`#![...]`).

## Browser JavaScript

Browser-page JavaScript execution via `adk-browser` is a separate capability. It runs inside the remote page context and is not part of the code execution substrate. Use `adk-browser` tools for web automation and page interaction.
