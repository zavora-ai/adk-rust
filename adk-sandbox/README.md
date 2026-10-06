# adk-sandbox

Isolated code execution runtime for [ADK-Rust](https://github.com/zavora-ai/adk-rust) agents.

`adk-sandbox` provides the `SandboxBackend` trait and two implementations for executing code in isolation. It separates the *isolation concern* from language-specific toolchains — `adk-code` handles compilation and language pipelines, while `adk-sandbox` handles running the resulting code safely.

## Feature Flags

| Feature   | Description                                | Default | Extra Dependencies |
|-----------|--------------------------------------------|---------|--------------------|
| `process` | Subprocess execution via `tokio::process`  | ✅      | None (uses tokio)  |
| `wasm`    | In-process WASM execution via `wasmtime`   | ❌      | `wasmtime`, `wasmtime-wasi` |
| `sandbox-macos` | macOS Seatbelt enforcement          | ❌      | None               |
| `sandbox-linux` | Linux bubblewrap enforcement        | ❌      | None (external `bwrap` binary) |
| `sandbox-windows` | Windows AppContainer enforcement  | ❌      | `windows-sys`      |
| `sandbox-native` | Auto-detect platform enforcer      | ❌      | All of the above   |
| `workspace` | Workspace lifecycle + `LocalUnixClient` | ❌   | `tar`, `uuid`      |
| `workspace-docker` | `DockerClient` (implies `workspace`) | ❌ | `bollard`, `futures` |

## Backend Comparison

| Capability              | `ProcessBackend`         | `WasmBackend`            |
|-------------------------|--------------------------|--------------------------|
| Timeout enforcement     | ✅ `tokio::time::timeout`, stdin write included | ✅ Epoch-based interruption |
| Memory limit            | ❌ Not enforced           | ✅ `StoreLimitsBuilder`: one linear memory, `memory_limit_mb` or 256 MiB by default |
| Network isolation       | Only with an OS enforcer | ✅ No WASI network        |
| Filesystem isolation    | Only with an OS enforcer | ✅ No WASI preopens       |
| Environment isolation   | ✅ `env_clear()` + explicit env | ✅ Full (no host access) |
| Output truncation       | ✅ 1 MB limit applied while reading, UTF-8 safe | ✅ 1 MB capture pipes |
| Process cleanup         | ✅ Process group killed on timeout and on exit | — (in-process) |
| Supported languages     | Rust, Python, JS, TS, Command | Wasm only            |

`ProcessBackend` is honest about what it does *not* enforce. Use `WasmBackend` when you need full sandboxing with memory limits and no host access, or attach an OS enforcer (below) to confine a `ProcessBackend`.

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

The tool accepts `language`, `code`, optional `stdin`, and optional `timeout_secs` parameters. It requires the `code:execute` scope. `timeout_secs` is clamped to the schema's 1–300 second range; a missing or non-numeric value uses 30 seconds.

## Error Handling

All backend errors use `SandboxError`:

| Variant            | When                                          |
|--------------------|-----------------------------------------------|
| `Timeout`          | Execution exceeded the configured timeout     |
| `MemoryExceeded`   | WASM module exceeded memory limit             |
| `ExecutionFailed`  | Internal error (I/O, spawn failure)           |
| `InvalidRequest`   | Unsupported language for this backend         |
| `BackendUnavailable` | Missing runtime or feature not enabled      |
| `EnforcerFailed`   | Sandbox enforcer failed to apply profile      |
| `EnforcerUnavailable` | Sandbox enforcer not functional on this system |
| `PolicyViolation`  | A policy path could not be resolved           |

Non-zero exit codes are **not** errors — they are returned in `ExecResult.exit_code`.

## OS Sandbox Profiles

OS-level sandbox enforcement restricts child processes at the kernel level — blocking network access, confining filesystem reads and writes to the policy's paths, and controlling process spawning. This goes beyond `ProcessBackend`'s default environment isolation.

Under an enforcer, `ProcessBackend` runs each execution in a fresh scratch directory that holds its source file and compiler output. The directory is the process's working directory and is granted read-write in addition to the policy's paths; nothing else from the host is added.

### Feature Flags

| Feature | Platform | Enforcer | Extra Dependencies |
|---------|----------|----------|--------------------|
| `sandbox-macos` | macOS | Seatbelt (`sandbox-exec`) | None |
| `sandbox-linux` | Linux | bubblewrap (`bwrap`) | None (external binary) |
| `sandbox-windows` | Windows | AppContainer | `windows-sys` |
| `sandbox-native` | Auto-detect | Platform-appropriate | All of the above |

### Usage

```rust
use adk_sandbox::{
    ProcessBackend, ProcessConfig, SandboxBackend,
    SandboxPolicyBuilder, get_enforcer,
};

// 1. Build a policy. Seatbelt grants the macOS system runtime itself; bubblewrap
//    mounts only the listed paths, so Linux policies list the system directories too.
let policy = SandboxPolicyBuilder::new()
    .allow_read("/usr")
    .allow_read("/tmp")
    .allow_read_write("/tmp/work")
    .allow_process_spawn()
    // Network is denied by default
    .env("PATH", "/usr/bin:/usr/local/bin")
    .build();

// 2. Get the platform enforcer
let enforcer = get_enforcer()?;
println!("Using enforcer: {}", enforcer.name());

// 3. Create a sandboxed backend
let backend = ProcessBackend::with_sandbox(
    ProcessConfig::default(),
    enforcer,
    policy,
);

// 4. Execute code — network is blocked, writes restricted
let result = backend.execute(request).await?;
```

### Platform Differences

| Aspect | macOS Seatbelt | Linux bubblewrap | Windows AppContainer |
|--------|---------------|-----------------|---------------------|
| Strategy | Deny-default profile plus the system runtime | Whitelist (mount only what's needed) | Not implemented — the enforcer reports itself unavailable |
| Reads | System runtime and policy paths; home directories, `/tmp`, and `$TMPDIR` are unreadable unless allowed | Only bound paths exist | — |
| Writes | Only policy read-write paths (and `/dev/null`) | Only `--bind` paths writable | — |
| Network | Denied unless `allow_network` | `--unshare-net` namespace | — |
| Process spawning | `process-fork` granted only with `allow_process_spawn` | seccomp filter fails `fork`, `vfork`, and non-thread `clone` (x86-64, AArch64) | — |
| Domain rules (`allow_domain`) | Not enforceable; all network blocked | Not enforceable; all network blocked | — |

The Seatbelt system runtime covers `/bin`, `/sbin`, `/usr`, `/System`, `/Library/Apple`, `/Library/Frameworks`, `/opt/homebrew`, `/private/etc` (minus `master.passwd` and `sudoers`), the dyld cache, and time zone data. File metadata (existence, size, timestamps) is readable everywhere because path resolution needs it; file contents are not. Programs installed elsewhere — a rustup toolchain, a pyenv interpreter, Xcode — need their directories passed to `allow_read`.

Every path is written into the Seatbelt profile as an escaped string literal, so a directory name containing `"`, `\`, or control characters cannot add directives to the profile.

## Workspace Clients

The `workspace` feature provides the `provision → session → exec → snapshot → resume` lifecycle.

| Control | `LocalUnixClient` | `DockerClient` |
|---------|-------------------|----------------|
| Filesystem | Host filesystem; the workspace is only the working directory | Container filesystem |
| Network | Host network | `none` by default; `with_network_mode("bridge")` to enable |
| Capabilities | Host user | `cap_drop: ALL`, `no-new-privileges` |
| Process count | Unlimited | 512 by default (`with_pids_limit`) |
| Environment | Cleared except `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR`, `TERM` | Image environment |
| Output | 1 MiB per stream, read while the command runs | Unbounded |
| Timeout | Kills the command's process group | Kills the command's process tree inside the container |

`LocalUnixClient` is not an isolation boundary. Use `DockerClient` when workspace commands are untrusted. `DockerClient` passes paths, URLs, and branch names as command arguments rather than shell text, reads files as raw bytes, and needs a network mode other than `none` for `GitRepo` manifest entries.

### Example

See [`examples/sandbox_agent/`](../examples/sandbox_agent/) for a full LLM-agent-driven example that executes Python code in a sandboxed environment with network access blocked.

## License

Apache-2.0
