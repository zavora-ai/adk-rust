# adk-sandbox

Isolated code execution runtime for [ADK-Rust](https://github.com/zavora-ai/adk-rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-sandbox.svg)](https://crates.io/crates/adk-sandbox)
[![Documentation](https://docs.rs/adk-sandbox/badge.svg)](https://docs.rs/adk-sandbox)
[![License](https://img.shields.io/crates/l/adk-sandbox.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

`adk-sandbox` provides the `SandboxBackend` trait and two implementations for executing code in isolation. It separates the *isolation concern* from language-specific toolchains — `adk-code` handles compilation and language pipelines, while `adk-sandbox` handles running the resulting code safely.

## Installation

```toml
[dependencies]
adk-sandbox = "3.0"

# Or, with OS-level enforcement and the WASM backend:
# adk-sandbox = { version = "3.0", features = ["sandbox-native", "wasm"] }
```

Or use the meta-crate, where the `sandbox` feature (included in `full`) re-exports it:

```toml
[dependencies]
adk-rust = { version = "3.0", features = ["sandbox"] }
```

## Feature Flags

| Feature   | Description                                | Default | Extra Dependencies |
|-----------|--------------------------------------------|---------|--------------------|
| `process` | Subprocess execution via `tokio::process`  | ✅      | None (uses tokio)  |
| `wasm`    | In-process WASM execution via `wasmtime`   | ❌      | `wasmtime`, `wasmtime-wasi` |
| `sandbox-macos` | macOS Seatbelt enforcement          | ❌      | None               |
| `sandbox-linux` | Linux bubblewrap enforcement        | ❌      | None (external `bwrap` binary) |
| `sandbox-windows` | Windows restricted-token enforcement | ❌      | `windows-sys`      |
| `sandbox-native` | All three enforcers; only the target platform's compiles | ❌ | All of the above |
| `process-tree` | `process_tree::JobObject` and `ContainedChild`: a process tree held in a Job Object on Windows (implied by `sandbox-windows`); `process_tree::ProcessGroup`: one held in a session and process group of its own on Unix | ❌ | `windows-sys` on Windows |
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
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::Llm;
use adk_sandbox::{ProcessBackend, SandboxTool};
use std::sync::Arc;

fn sandbox_agent(model: Arc<dyn Llm>) -> adk_core::Result<LlmAgent> {
    let backend = Arc::new(ProcessBackend::default());
    let tool = SandboxTool::new(backend);

    LlmAgentBuilder::new("sandbox_agent").model(model).tool(Arc::new(tool)).build()
}
```

The tool accepts `language`, `code`, optional `stdin`, and optional `timeout_secs` parameters. It requires the `code:execute` scope. `timeout_secs` is clamped to the schema's 1–300 second range; a missing or non-numeric value uses 30 seconds.

## Error Handling

All backend errors use `SandboxError`, which is `#[non_exhaustive]`: a `match` outside the
crate needs a wildcard arm.

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
| `ProvisionFailed`, `SessionNotFound`, `SnapshotNotFound`, `PathTraversal`, `DockerUnavailable`, `SessionTimeout` | Workspace lifecycle failures (`workspace` feature only) |

Non-zero exit codes are **not** errors — they are returned in `ExecResult.exit_code`.

## OS Sandbox Profiles

OS-level sandbox enforcement restricts child processes at the kernel level — blocking network access, confining filesystem reads and writes to the policy's paths, and controlling process spawning. This goes beyond `ProcessBackend`'s default environment isolation. The Windows restricted-token enforcer restricts neither the network nor process creation, and refuses a policy that denies the network unless the caller acknowledges it stays open (see [Platform Differences](#platform-differences)).

Under an enforcer, `ProcessBackend` runs each execution in a fresh scratch directory that holds its source file and compiler output. The directory is the process's working directory and is granted read-write in addition to the policy's paths; nothing else from the host is added.

### Feature Flags

| Feature | Platform | Enforcer | Extra Dependencies |
|---------|----------|----------|--------------------|
| `sandbox-macos` | macOS | Seatbelt (`sandbox-exec`) | None |
| `sandbox-linux` | Linux | bubblewrap (`bwrap`) | None (external binary) |
| `sandbox-windows` | Windows | Restricted token (`WindowsEnforcer::run_confined`, `spawn_contained`) | `windows-sys` |
| `sandbox-native` | Auto-detect | Platform-appropriate | All of the above |

### Usage

```rust
use adk_sandbox::{
    ExecRequest, ExecResult, ProcessBackend, ProcessConfig, SandboxBackend, SandboxError,
    SandboxPolicyBuilder, get_enforcer,
};

async fn run_confined(request: ExecRequest) -> Result<ExecResult, SandboxError> {
    // 1. Build a policy. Seatbelt grants the macOS system runtime itself; bubblewrap
    //    mounts only the listed paths, so Linux policies list the system directories too.
    //    Every allowed path must exist, or execution fails with `PolicyViolation`.
    let policy = SandboxPolicyBuilder::new()
        .allow_read("/usr")
        .allow_read("/tmp")
        .allow_read_write("/tmp/work")
        .allow_process_spawn()
        // Network is denied by default
        .env("PATH", "/usr/bin:/bin")
        .build();

    // 2. Get the platform enforcer (needs `sandbox-macos`, `sandbox-linux`, or `sandbox-native`)
    let enforcer = get_enforcer()?;
    println!("Using enforcer: {}", enforcer.name());

    // 3. Create a sandboxed backend
    let backend = ProcessBackend::with_sandbox(ProcessConfig::default(), enforcer, policy);

    // 4. Execute code — network is blocked, writes restricted
    backend.execute(request).await
}
```

### Running a Command in a Workspace

`sandbox::sandboxed_command` builds the `tokio::process::Command` for one program under an
enforcer and a policy, starting in a directory the caller names — the shell of a coding
harness runs in the user's workspace rather than in a scratch directory.

| Aspect | Value |
|--------|-------|
| Program | The enforcer's wrapper (`sandbox-exec`, `bwrap`), resolved on the caller's `PATH` |
| Arguments | The wrapper's arguments, then the program and its arguments |
| Working directory | The `cwd` argument; it is not granted, so list it in the policy |
| Environment | Cleared, then exactly `policy.env` |
| Streams, process group, kill-on-drop | Left to the caller |

```rust
use std::ffi::{OsStr, OsString};
use std::path::Path;

use adk_sandbox::sandbox::{SandboxPolicyBuilder, get_enforcer, sandboxed_command};
use adk_sandbox::SandboxError;

async fn run_in_workspace(workspace: &Path, scratch: &Path) -> Result<String, SandboxError> {
    let policy = SandboxPolicyBuilder::new()
        .allow_read_write(workspace)
        .allow_read(workspace.join(".git")) // read-only inside the writable workspace
        .allow_read_write(scratch)
        .allow_process_spawn() // `sh -c` forks
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch.display().to_string())
        .build();
    let enforcer = get_enforcer()?;

    let output = sandboxed_command(
        enforcer.as_ref(),
        OsStr::new("sh"),
        &[OsString::from("-c"), OsString::from("cargo test --quiet")],
        &policy,
        workspace,
    )?
    .stdin(std::process::Stdio::null())
    .output()
    .await?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
```

### Read-Only Paths Inside a Writable Path

When allowed paths nest, the most specific entry decides, whatever order the policy lists
them in; for the same path, read-write wins. A read-only entry inside a read-write entry
refuses every write, creation, removal, rename, and hard or symbolic link beneath it, and
the writable directories above it cannot be renamed or removed, so the entry cannot be
carried away from its path. Both enforcers behave the same way, and
`tests/nested_read_only_conformance_tests.rs` runs one script set under each.

| | macOS Seatbelt | Linux bubblewrap |
|---|---|---|
| Mechanism | Rules emitted shallowest first; `(deny file-write* (subpath ...))` for the entry; `(deny file-write-unlink (literal ...))` for each writable directory above it | Mounts made shallowest first, so the read-only bind covers its parent's; each writable directory above it bound onto itself, which makes it a mount point |
| A write beneath the entry | `Operation not permitted` | `Read-only file system` |
| Renaming or removing the entry or a directory above it | `Operation not permitted` | `Resource busy` or `Invalid cross-device link`; GNU `mv` then copies, and the original stays in place |
| A hard link to a file beneath it | `Operation not permitted` | `Invalid cross-device link` |

The entry must exist when the command is built, as every allowed path must; a path that
does not exist yet cannot be protected this way.

> **Note:** bubblewrap mounts each allowed path at its resolved location. On a merged-`/usr`
> distribution `/bin` and `/lib` are symbolic links into `/usr`, so a policy that lists them
> does not mount the dynamic loader's `/lib` path and dynamically linked programs fail with
> `execvp: No such file or directory`. Allow `/` read-only on such hosts; under a `Denied`
> network `/run`, `/tmp`, `/var/tmp` and `/dev/shm` are then covered with empty read-only
> `tmpfs` mounts, so their Unix sockets stay out of reach. A socket elsewhere under a granted
> path, such as in a home directory, stays reachable.

### Platform Differences

| Aspect | macOS Seatbelt | Linux bubblewrap | Windows restricted token |
|--------|---------------|-----------------|---------------------|
| Strategy | Deny-default profile plus the system runtime | Whitelist (mount only what's needed) | Restricted, Low-integrity token plus ACL grants and Low labels, through `WindowsEnforcer::run_confined` and `spawn_contained` only (probed by `probe_confined`); `probe` reports the `sandboxed_command` path unavailable |
| Reads | System runtime and policy paths; home directories, `/tmp`, and `$TMPDIR` are unreadable unless allowed | Only bound paths exist | What the user may read (the user is a restricting SID, since Git for Windows' MSYS2 runtime needs it); paths only `Administrators` may read are refused |
| Writes | Only policy read-write paths (and `/dev/null`) | Only `--bind` paths writable | Policy read-write paths, labelled Low; paths already labelled Low, such as `AppData\LocalLow` |
| Network (`SandboxPolicy::network`) | `Denied`: `(deny network*)`, Unix sockets included; `Proxied`: only the egress proxy's `localhost:<port>`; `Open`: all | `Denied`: `--unshare-net` (IP traffic and abstract sockets), with `/run`, `/tmp`, `/var/tmp` and `/dev/shm` kept out of reach; `Proxied`: refused; `Open`: host network | Not enforced: `Denied` and `Proxied` are refused with `Unenforceable` unless the enforcer is built with `acknowledge_open_network()` |
| MSYS2 programs (Git for Windows' `sh`) | — | — | Run from a private copy of the runtime per call, apart from the user's own; the user's namespaces of the program's installation and those on `PATH` are held for the call, other installations' are not |
| Process spawning | `process-fork` granted only with `allow_process_spawn` | seccomp filter fails `fork`, `vfork`, and non-thread `clone` (x86-64, AArch64) | Not restricted; every descendant is killed with the call's Job Object |
| Terminal | The caller's session | Always a new session (`--new-session`) with no controlling terminal | No console is attached; standard streams are not captured |

The Seatbelt system runtime covers `/bin`, `/sbin`, `/usr`, `/System`, `/Library/Apple`, `/Library/Frameworks`, `/opt/homebrew`, `/private/etc` (minus `master.passwd` and `sudoers`), the dyld cache, and time zone data. File metadata (existence, size, timestamps) is readable everywhere because path resolution needs it; file contents are not. Programs installed elsewhere — a rustup toolchain, a pyenv interpreter, Xcode — need their directories passed to `allow_read`.

Every path is written into the Seatbelt profile as an escaped string literal, so a directory name containing `"`, `\`, or control characters cannot add directives to the profile.

## Workspace Clients

The `workspace` feature provides the `provision → session → exec → snapshot → resume` lifecycle.

| Control | `LocalUnixClient` | `DockerClient` |
|---------|-------------------|----------------|
| Commands' filesystem | Host filesystem; the workspace is only the working directory | Container filesystem |
| Session file operations | `read_file`, `write_file`, `list_dir`, `apply_patch` headers, and the working directory resolve inside the workspace, following symlinks only within it; a dangling symlink is refused | Inside the container's `/workspace` |
| Network | Host network | `none` by default; `with_network_mode("bridge")` to enable |
| Capabilities | Host user | `cap_drop: ALL`, `no-new-privileges` |
| Process count | Unlimited | 512 by default (`with_pids_limit`) |
| Environment | Cleared except `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR`, `TERM` | Image environment |
| Output | 1 MiB per stream, read while the command runs | Unbounded |
| Timeout | Kills the command's process group | Kills the command's process tree inside the container |
| Host directories | The host filesystem | None by default; `with_bind_mount(host, container, read_only)` mounts one into every container |
| User | Host user | The image's default (usually `root`, which without capabilities cannot write a host directory only its owner may write); `with_user("1000:1000")` runs commands as that user, so a bind-mounted workspace is writable and what the container writes belongs to the host user |

`DockerClient::with_bind_mount("/home/me/project", "/workspace", false)` gives commands in
the container and file tools on the host one view of the workspace. A read-only mount
refuses writes from inside the container, whose dropped capabilities keep it from being
remounted. Content under a mount stays on the host, so `snapshot` does not capture it.

Session paths are relative: an absolute path or a `..` that leaves the root fails with
`SandboxError::PathTraversal`. Writing a file with more than one hard link replaces it, keeping
its permissions, instead of truncating it, so a link outside the workspace keeps its contents.

`LocalUnixClient` is not an isolation boundary for commands. Use `DockerClient` when workspace commands are untrusted. `DockerClient` passes paths, URLs, and branch names as command arguments rather than shell text, reads files as raw bytes, and needs a network mode other than `none` for `GitRepo` manifest entries. A provisioning that fails after its container was created removes that container before the error is returned.

### Example

See [`examples/sandbox_agent/`](https://github.com/zavora-ai/adk-rust/tree/main/examples/sandbox_agent) for a full LLM-agent-driven example that executes Python code in a sandboxed environment with network access blocked.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
