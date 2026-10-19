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

- **How the program is found.** Without an enforcer, a bare `python3`, `node`, or
  `rustc` is looked up on the caller's `PATH` and passed to the child as an absolute
  path, so the child needs no `PATH` of its own. Under an enforcer only the wrapper
  (`sandbox-exec` or `bwrap`) is resolved that way; the program inside it is found
  through the child's `PATH`, which comes from the policy's or the request's `env`.
  Set `ProcessConfig::python_path` and `node_path` to absolute paths to avoid the lookup.
- **Compilation runs through the same boundary as execution.** Compilation is not
  inert: `include_str!` reads files and procedural macros run arbitrary code before the
  produced binary exists, so the compile runs with the enforcer wrapper, the timeout,
  and a process group. The compile phase receives a platform-specific toolchain
  allow-list; on Windows this includes the MSVC and Windows SDK paths, discovered
  from the installed toolchain when the caller is not already in a Developer shell.
  Compilation uses the Rust toolchain's `rust-lld` linker so an unrelated `link.exe`
  earlier on `PATH` cannot be selected. An OS enforcer is what constrains that phase.

### Environment Precedence

`SandboxPolicy::env` supplies defaults for every execution and `ExecRequest::env`
overrides them per call.

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
    ExecRequest, Language, ProcessBackend, ProcessConfig, SandboxBackend,
    SandboxPolicyBuilder, get_enforcer,
};
use std::collections::HashMap;
use std::time::Duration;

// Policy paths must exist: the enforcer resolves them when it builds the profile.
std::fs::create_dir_all("/tmp/work")?;

// 1. Define what the sandboxed process can do. Seatbelt adds the macOS system
//    runtime itself; bubblewrap mounts only what is listed.
let policy = SandboxPolicyBuilder::new()
    .allow_read("/usr")            // Read system libraries
    .allow_read_write("/tmp/work") // Write to work directory
    // Network and process creation are denied by default.
    // The interpreter is found through this PATH inside the sandbox.
    .env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin")
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
let request = ExecRequest {
    language: Language::Python,
    code: "print(1 + 1)".to_string(),
    stdin: None,
    timeout: Duration::from_secs(30),
    memory_limit_mb: None,
    env: HashMap::new(),
};
let result = backend.execute(request).await?;
assert_eq!(result.stdout.trim(), "2");
```

> **Note:** On macOS, `/usr/bin/python3` is a shim that loads the active Xcode or Command
> Line Tools, which the Seatbelt profile does not grant; the call then exits with an
> `xcrun` error. Put a standalone interpreter first on the policy's `PATH` (Homebrew's
> `/opt/homebrew/bin` is granted), or `allow_read` the developer directory.

Under an enforcer each execution runs in a fresh scratch directory that holds its source
file and compiler output. It is the working directory and is granted read-write in
addition to the policy's paths.

### Feature Flags

```toml
[dependencies]
# Auto-detect platform enforcer
adk-sandbox = { version = "3.0.0", features = ["process", "sandbox-native"] }

# Or pick a specific platform
adk-sandbox = { version = "3.0.0", features = ["process", "sandbox-macos"] }
adk-sandbox = { version = "3.0.0", features = ["process", "sandbox-linux"] }
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
| File operations | `read_file`, `write_file`, `list_dir`, `apply_patch`, and the working directory resolve inside the workspace, through symlinks; a hard-linked file is replaced on write, not truncated | Container filesystem |
| Network | Host network | `none` by default; `with_network_mode("bridge")` to enable |
| Privileges | Host user | `cap_drop: ALL`, `no-new-privileges`, 512-process limit (`with_pids_limit`) |
| Environment | Cleared except `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TZ`, `TMPDIR`, `TERM` | Image environment |
| Timeout | Kills the command's process group | Kills the command's process tree inside the container |

`LocalUnixClient` is not an isolation boundary; use `DockerClient` for untrusted commands.
`GitRepo` manifest entries need a `DockerClient` network mode other than `none`.

A `LocalUnixSession` file operation fails with `SandboxError::PathTraversal` when its
path leaves the workspace: an absolute path, a `..` that escapes, a symlink that resolves
outside, a dangling or looping symlink, a file named in an `apply_patch` header, or a
manifest entry. Symlinks that stay inside the workspace are allowed. The check is not a
lock — a process that can create symlinks in the workspace can race it — and commands run
with `exec_command` are not confined.

`SandboxError` is `#[non_exhaustive]`, and its workspace variants, such as
`PathTraversal`, exist only with the `workspace` feature, so a `match` on it outside the
crate needs a wildcard arm.

### Example

See [`examples/sandbox_agent/`](https://github.com/zavora-ai/adk-rust/tree/main/examples/sandbox_agent) for a full LLM-agent-driven example that executes Python code in a sandboxed environment with network access blocked by the OS kernel.
