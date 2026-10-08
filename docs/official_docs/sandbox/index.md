# Sandboxed Code Execution

The `adk-sandbox` crate provides isolated code execution for ADK agents, with two levels of isolation:

1. **Process isolation** — child processes with environment isolation and timeout enforcement
2. **OS-level sandbox profiles** — kernel-level restrictions on filesystem, network, and process spawning

## Backends

| Backend | Isolation Level | Languages | Feature Flag |
|---------|----------------|-----------|--------------|
| `ProcessBackend` | Environment + timeout | Rust, Python, JS, TS, Command | `process` (default) |
| `ProcessBackend` + sandbox | Kernel-level | Same as above | `process` + `sandbox-native` |
| `WasmBackend` | Full (memory, fs, network) | WASM only | `wasm` |

### Which Isolation Are You Getting?

`ProcessBackend::isolation()` reports it, so this is not something to infer from the
crate name:

| Result | Meaning |
|--------|---------|
| `IsolationClass::SubprocessOnly` | A child process with a cleared environment, a timeout, and its own process group, which is killed on timeout and again when the process exits. The OS applies no further restriction: the code can read the host filesystem and reach the network. This is what `ProcessBackend::default()` gives you. |
| `IsolationClass::OsEnforced` | An enforcer **and** a policy are attached, so the OS restricts the child. |

Two things about the process backend worth knowing:

- **Programs are resolved before the environment is cleared.** A bare `python3`, `node`,
  or `rustc` is looked up on the caller's `PATH` and passed to the child as an absolute
  path. That means the child does not need a `PATH` of its own to start — previously a
  caller had to put `PATH` in `ExecRequest::env`, which also let the executed code spawn
  anything else on it.
- **Compilation runs through the same boundary as execution.** Rust source used to be
  compiled by a command built outside the shared path, so the compile had no enforcer
  wrapper, no timeout, and no process group. That matters because compilation is not
  inert: `include_str!` reads files and procedural macros run arbitrary code before the
  produced binary exists. The compile phase receives a platform-specific toolchain
  allow-list; on Windows this includes the MSVC and Windows SDK paths, discovered
  from the installed toolchain when the caller is not already in a Developer shell.
  Compilation uses the Rust toolchain's `rust-lld` linker so an unrelated `link.exe`
  earlier on `PATH` cannot be selected. An OS enforcer is what constrains that phase.

### Environment Precedence

`SandboxPolicy::env` supplies defaults for every execution and `ExecRequest::env`
overrides them per call. The policy's variables were previously ignored entirely.

### Input, Output, and Lifetime

| Concern | Behaviour |
|---------|-----------|
| stdin | Written concurrently with the output reads and inside the timeout, so a child that never reads it cannot hang the call |
| stdout, stderr | Read while the child runs and capped at `ProcessConfig::max_output_bytes` (1 MiB) each; the excess is drained and a truncation notice appended |
| Descendants | The execution's process group is killed on timeout and when the main process exits, so `nohup … &` jobs do not outlive the call |
| `SandboxTool` timeout | `timeout_secs` is clamped to the schema's 1–300 seconds |

## OS Sandbox Profiles

OS-level sandbox enforcement restricts child processes at the kernel level. This goes beyond environment isolation — the OS itself blocks unauthorized filesystem access, network connections, and process spawning.

### Platform Support

| Platform | Enforcer | How It Works |
|----------|----------|-------------|
| macOS | Seatbelt (`sandbox-exec`) | Deny-by-default profile: the system runtime plus the policy's paths are readable, only the policy's read-write paths are writable, network and fork only when allowed |
| Linux | bubblewrap (`bwrap`) | Filesystem namespace isolation (whitelist mounts); a seccomp filter denies process creation unless allowed |
| Windows | AppContainer | **Not implemented** — the enforcer reports itself unavailable |

### Quick Start

```rust
use adk_sandbox::{
    ProcessBackend, ProcessConfig, SandboxBackend,
    SandboxPolicyBuilder, get_enforcer,
};

// 1. Define what the sandboxed process can do. Seatbelt adds the macOS system
//    runtime itself; bubblewrap mounts only what is listed.
let policy = SandboxPolicyBuilder::new()
    .allow_read("/usr")           // Read system libraries
    .allow_read_write("/tmp/work") // Write to work directory
    .allow_process_spawn()         // Python needs to exec
    // Network is denied by default
    .env("PATH", "/usr/bin:/usr/local/bin")
    .build();

// 2. Get the platform-appropriate enforcer
let enforcer = get_enforcer()?;

// 3. Create a sandboxed backend
let backend = ProcessBackend::with_sandbox(
    ProcessConfig::default(),
    enforcer,
    policy,
);

// 4. Execute code — network is blocked, reads and writes restricted
let result = backend.execute(request).await?;
```

Under an enforcer each execution runs in a fresh scratch directory that holds its source
file and compiler output. It is the working directory and is granted read-write in
addition to the policy's paths.

### Feature Flags

```toml
[dependencies]
# Auto-detect platform enforcer
adk-sandbox = { version = "2.3.0", features = ["process", "sandbox-native"] }

# Or pick a specific platform
adk-sandbox = { version = "2.3.0", features = ["process", "sandbox-macos"] }
adk-sandbox = { version = "2.3.0", features = ["process", "sandbox-linux"] }
```

### SandboxPolicy

The policy defines what a sandboxed process is allowed to do:

| Field | Default | Description |
|-------|---------|-------------|
| `allowed_paths` | `[]` (deny all) | Filesystem paths with read-only or read-write access |
| `allow_network` | `false` | Whether network access is permitted |
| `network_rules` | `[]` | Intended domains from `allow_domain`. **Not enforced on any platform**; with `allow_network = false` all network stays blocked |
| `allow_process_spawn` | `false` | Whether creating child processes is permitted; `execve` in place is always allowed |
| `env` | `{}` | Environment variables for the sandboxed process |

### Platform Differences

**macOS (Seatbelt):** Deny-by-default. The profile grants reads of the system runtime
(`/bin`, `/sbin`, `/usr`, `/System`, `/Library/Apple`, `/Library/Frameworks`,
`/opt/homebrew`, `/private/etc` minus `master.passwd` and `sudoers`, the dyld cache, time
zone data), the mach services process startup needs, and the policy's paths. Home
directories, `/tmp`, and `$TMPDIR` are unreadable unless allowed, so a rustup toolchain,
a pyenv interpreter, or Xcode must be passed to `allow_read`. File metadata is visible
everywhere because path resolution needs it; file contents are not. Paths are written into
the profile as escaped string literals, so a directory name cannot add directives.

**Linux (bubblewrap):** Uses namespace-based whitelist — nothing exists by default, you mount only what's needed. Without `allow_process_spawn`, a seccomp filter fails `fork`, `vfork`, and non-thread `clone` (x86-64 and AArch64; other architectures return `EnforcerFailed`). Install with `apt install bubblewrap` or `dnf install bubblewrap`.

**Domain rules:** Seatbelt network filters accept only `*` or `localhost` as a host, and
bubblewrap has no domain-level control, so `allow_domain` cannot be enforced. A policy
with rules and `allow_network = false` blocks all network access and logs a warning.

**Windows (AppContainer):** Not implemented. The design is token-based ACLs — a restricted SID with no access by default, then ACLs granted on specific paths — but container creation, ACLs, capabilities, and job-object cleanup are absent, so `probe()` returns `EnforcerUnavailable`. Run without an enforcer on Windows, or use macOS or Linux where enforcement is real.

## Workspace Clients

The `workspace` feature adds a `provision → session → exec → snapshot → resume` lifecycle.

| Control | `LocalUnixClient` | `DockerClient` (`workspace-docker`) |
|---------|-------------------|-------------------------------------|
| Filesystem | Host filesystem; the workspace is only the working directory | Container filesystem |
| Network | Host network | `none` by default; `with_network_mode("bridge")` to enable |
| Privileges | Host user | `cap_drop: ALL`, `no-new-privileges`, 512-process limit (`with_pids_limit`) |
| Environment | Cleared except `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR`, `TERM` | Image environment |
| Timeout | Kills the command's process group | Kills the command's process tree inside the container |

`LocalUnixClient` is not an isolation boundary; use `DockerClient` for untrusted commands.
`GitRepo` manifest entries need a `DockerClient` network mode other than `none`.

### Example

See [`examples/sandbox_agent/`](https://github.com/zavora-ai/adk-rust/tree/main/examples/sandbox_agent) for a full LLM-agent-driven example that executes Python code in a sandboxed environment with network access blocked by the OS kernel.
