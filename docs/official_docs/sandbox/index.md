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
  On macOS, a program that resolves to an Xcode shim in `/usr/bin` (`python3`, `git`,
  `clang`) is replaced by the tool `xcrun --find` names, and the active developer
  directory (`xcode-select --print-path`) is granted read-only for that execution.
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
| Windows | Restricted token (`restricted-token`) | A restricted token plus ACL grants on the policy's paths, in a Job Object, through `WindowsEnforcer::run_confined`; not through `sandboxed_command` (see below) |

Enforcement is proven on a real Linux runner by the `Sandbox Linux` workflow
(`.github/workflows/sandbox-linux.yml`): it installs bubblewrap on `ubuntu-24.04`, sets
`kernel.apparmor_restrict_unprivileged_userns=0` (the image restricts unprivileged user
namespaces through AppArmor), and runs the sandbox and harness shell suites with
`ADK_SANDBOX_REQUIRE_ENFORCER=1`. The enforcement tests skip with a note on a host whose
enforcer cannot start; with that variable set they fail instead, so a runner where
bubblewrap silently stopped confining processes turns the job red. It runs on every push
to `main`, on a pull request that touches the sandbox or the harness shell, and on demand.

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

> **Note:** On macOS, `/usr/bin/python3` is a shim that runs the Python of the active Xcode
> or Command Line Tools through `libxcrun`, which cannot run under Seatbelt. When the
> program resolves to such a shim, `ProcessBackend` and `sandboxed_command` run the interpreter behind it
> directly and grants read-only access to the developer directory, which holds Apple's
> toolchain and no user data. A shim invoked from inside a script (`sh -c "python3 …"`)
> is not resolved this way; put a standalone interpreter first on the policy's `PATH`
> (Homebrew's `/opt/homebrew/bin` is granted), or `allow_read` the developer directory.

Under an enforcer each execution runs in a fresh scratch directory that holds its source
file and compiler output. It is the working directory and is granted read-write in
addition to the policy's paths.

### Running a Command in a Workspace

`ProcessBackend` runs code in a scratch directory. A harness shell runs in the user's
workspace instead: `adk_sandbox::sandbox::sandboxed_command` builds the
`tokio::process::Command` for one program under an enforcer and a policy, starting in the
directory the caller names. `ProcessBackend` builds its own sandboxed commands through it.

| Aspect | Value |
|--------|-------|
| Program | The enforcer's wrapper (`sandbox-exec`, `bwrap`), resolved on the caller's `PATH`; the program inside it is found through `policy.env`'s `PATH` |
| Arguments | The wrapper's arguments, then the program and its arguments |
| Working directory | The `cwd` argument. It is not granted: list it in `allowed_paths` |
| Environment | Cleared, then exactly `policy.env` — set `HOME` and `TMPDIR` to a scratch directory to keep the host's out of reach |
| Spawn-time state | What `SandboxEnforcer::configure_command` installs, such as the Linux seccomp descriptor |
| Streams, process group, kill-on-drop | Left to the caller |

```rust
use std::ffi::{OsStr, OsString};
use std::path::Path;

use adk_sandbox::SandboxError;
use adk_sandbox::sandbox::{SandboxPolicyBuilder, get_enforcer, sandboxed_command};

async fn run_in_workspace(workspace: &Path, scratch: &Path) -> Result<String, SandboxError> {
    let policy = SandboxPolicyBuilder::new()
        .allow_read_write(workspace)
        .allow_read(workspace.join(".git")) // read-only inside the writable workspace
        .allow_read_write(scratch)
        .allow_process_spawn() // `sh -c` forks
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", scratch.display().to_string())
        .env("TMPDIR", scratch.display().to_string())
        .build();
    let enforcer = get_enforcer()?;

    let output = sandboxed_command(
        enforcer.as_ref(),
        OsStr::new("sh"),
        &[OsString::from("-c"), OsString::from("git status --short")],
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
carried away from its path and the path recreated writable. A read-write entry inside the
read-only one is writable again, and fixed in place.

| | macOS Seatbelt | Linux bubblewrap |
|---|---|---|
| Mechanism | Rules emitted shallowest first, so the last match is the most specific; `(deny file-write* (subpath ...))` for the read-only entry, since allow rules add up; `(deny file-write-unlink (literal ...))` for each writable directory above it | Mounts made shallowest first, so the read-only bind covers its parent's; each writable directory above the entry bound onto itself, which makes it a mount point the kernel will not rename |
| A write beneath the entry | `Operation not permitted` | `Read-only file system` |
| Renaming or removing the entry or a directory above it | `Operation not permitted` | `Resource busy` or `Invalid cross-device link`; GNU `mv` then copies, and the original stays in place |
| A hard link to a file beneath it | `Operation not permitted` | `Invalid cross-device link` |

`adk-sandbox/tests/nested_read_only_conformance_tests.rs` runs the same scripts under both
enforcers (under bubblewrap in CI through the `Sandbox Linux` workflow). The read-only entry
must exist when the command is built, as every allowed path must, so a path that does not
exist yet cannot be protected from creation this way.

> **Note:** bubblewrap mounts each allowed path at its resolved location. On a merged-`/usr`
> distribution `/bin` and `/lib` are symbolic links into `/usr`, so a policy that lists them
> does not mount the dynamic loader's `/lib` path, and dynamically linked programs fail to
> start with `execvp: No such file or directory`. Allow `/` read-only on such hosts.

### The Harness Shell

`adk-harness` runs its `bash` tool under these enforcers when the harness is built with
`ShellIsolation::OsSandbox`. Its `SandboxLauncher` is a devtools `CommandLauncher` over
`sandboxed_command`:

| Policy entry | Read-write (Ask, Auto) | Read-only (Plan, ReadOnly) |
|---|---|---|
| Workspace root | `ReadWrite`, with each existing protected path (`.git`, `AGENTS.md`, …) `ReadOnly` inside it | `ReadOnly` |
| Per-session scratch directory, also `HOME` and `TMPDIR` | `ReadWrite` | `ReadWrite` |
| Toolchain directories (`PATH`, Cargo, rustup, Homebrew, …) | `ReadOnly` | `ReadOnly` |

On bubblewrap the launcher recreates a symbolically linked toolchain path such as `/bin` or
`/lib64` with `--symlink` and mounts `/dev` and `/proc`, so a merged-`/usr` host needs no
read-only `/`. On macOS it puts the active developer directory's tools first on `PATH`, since
the Xcode shims cannot run under Seatbelt. The harness probes the enforcer when it is built
and on every call, refuses rather than falling back to the host, and runs the nested
read-only conformance script above before it lets sandboxed commands run unapproved. See
[Harness Shell and Sandbox](../harness/shell-and-sandbox.md).

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

**Linux (bubblewrap):** Uses namespace-based whitelist — nothing exists by default, you mount only what's needed. Every command runs in a new session (`--new-session`) with no controlling terminal, so it cannot inject input into the caller's terminal with `TIOCSTI`. Without `allow_process_spawn`, a seccomp filter fails `fork`, `vfork`, and non-thread `clone` (x86-64 and AArch64; other architectures return `EnforcerFailed`). Install with `apt install bubblewrap` or `dnf install bubblewrap`.

**Domain rules:** Seatbelt network filters accept only `*` or `localhost` as a host, and
bubblewrap has no domain-level control, so `allow_domain` cannot be enforced. A policy
with rules and `allow_network = false` blocks all network access and logs a warning.

**Windows (restricted token):** the first Windows tier (spec 3.2 §10 W5). `WindowsEnforcer::run_confined` starts the command with `CreateProcessAsUserW` under a restricted token and holds it in a Job Object; AppContainer is not used.

| Aspect | Behaviour |
|--------|-----------|
| Token | Privileges removed (`DISABLE_MAX_PRIVILEGE`); the user SID and widening groups, including `Administrators`, are deny-only. Public runtime groups remain enabled. `WRITE_RESTRICTED` applies `RESTRICTED` and the logon SID to writes |
| Paths | `RESTRICTED` and the logon SID are granted on each policy path — full access for read-write, read and execute for read-only — added to the existing ACL |
| Writes | Require normal access plus a `RESTRICTED` or logon ACL grant; ordinary public `Users`/`Everyone` write access is insufficient |
| Reads | Owner-only private files require a policy grant; enabled public groups such as `Users` and `Everyone` still admit public files and system runtime dependencies. Reads are not a path allowlist |
| Descendants | Killed with the call's Job Object, also on a timeout |
| Network | **Not enforced** — Windows has no per-process network control without AppContainer |
| Integrity level | Not lowered |

Path grants are merged into existing ACLs and persist after a call, as before. The logon grant uses the caller's existing logon SID and the declared access mode; it grants no other user or logon access. Reusing a path does not revoke grants from earlier calls.

A restricted token is set when the process is created, so it cannot be attached to a `tokio::process::Command` that the caller spawns. `probe()` therefore returns `EnforcerUnavailable` and `configure_command()` returns `EnforcerFailed`, naming `run_confined`: `sandboxed_command`, `ProcessBackend::with_sandbox` and the harness shell do not confine on Windows and report so. The PR tier's `windows` job runs the enforcer's tests.

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
| Host directories | The host filesystem | None by default; `with_bind_mount(host, container, read_only)` mounts one into every container |
| User | Host user | The image's default (usually `root`, which without capabilities cannot write a host directory only its owner may write); `with_user("1000:1000")` runs commands as that user, so a bind-mounted workspace is writable and what the container writes belongs to the host user |

`DockerClient::with_bind_mount` gives commands in the container and file tools on the host
one view of a workspace:

```rust
use adk_sandbox::workspace::{DockerClient, Manifest, SandboxClient};
use adk_sandbox::SandboxError;

async fn workspace_container() -> Result<(), SandboxError> {
    let client = DockerClient::with_image("rust:1.95-slim")
        .await?
        .with_bind_mount("/home/me/project", "/workspace", false)
        .with_bind_mount("/home/me/.cargo/registry", "/usr/local/cargo/registry", true);
    let handle = client.provision(&Manifest { entries: vec![] }).await?;
    let session = client.start(&handle).await?;
    let output = session.exec_command("cargo test --offline", None).await?;
    println!("{}", output.stdout);
    client.stop(&handle).await
}
```

Both paths must be absolute and the host path must exist; Docker refuses anything else when
the container is created, and provisioning fails with `ProvisionFailed`. A read-only mount
refuses writes from inside the container, whose dropped capabilities keep it from being
remounted. Content under a mount stays on the host, so `snapshot` does not capture it and
`resume` mounts the same host paths again.

`LocalUnixClient` is not an isolation boundary; use `DockerClient` for untrusted commands.
`GitRepo` manifest entries need a `DockerClient` network mode other than `none`. A
provisioning that fails after its container was created removes that container before the
error is returned.

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
