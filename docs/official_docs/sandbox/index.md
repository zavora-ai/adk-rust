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

OS-level sandbox enforcement restricts child processes at the kernel level. This goes beyond environment isolation — the OS itself blocks unauthorized filesystem access, network connections, and process spawning. The Windows restricted-token enforcer restricts neither the network nor process creation, and refuses a policy that denies the network unless the caller acknowledges it stays open (see [Network Access](#network-access)).

### Platform Support

| Platform | Enforcer | How It Works |
|----------|----------|-------------|
| macOS | Seatbelt (`sandbox-exec`) | Deny-by-default profile: the system runtime plus the policy's paths are readable, only the policy's read-write paths are writable, network (all, or only an egress proxy's port) and fork only when allowed |
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
> start with `execvp: No such file or directory`. Allow `/` read-only on such hosts. Under a
> `Denied` network, `/run`, `/tmp`, `/var/tmp` and `/dev/shm` are then covered with empty,
> read-only `tmpfs` mounts, so the Unix sockets there stay out of reach (see
> [Network Access](#network-access)).

### The Harness Shell

`adk-harness` runs its `bash` tool under these enforcers when the harness is built with
`ShellIsolation::OsSandbox`. Its `SandboxLauncher` is a devtools `CommandLauncher` over
`sandboxed_command`:

| Policy entry | Read-write (Ask, Auto) | Read-only (Plan, ReadOnly) |
|---|---|---|
| Workspace root | `ReadWrite`, with each existing protected path (`.git/hooks`, `.git/config`, `.git/info`, `AGENTS.md`, …) `ReadOnly` inside it | `ReadOnly` |
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
| `network` | `NetworkAccess::Denied` | What the process may reach: `Denied` (nothing), `Proxied { endpoint }` (only an egress proxy's loopback port), or `Open` (everything). Set with `network`, `proxied` or `allow_network` on the builder; see [Network Access](#network-access) |
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

### Network Access

`SandboxPolicy::network` is one of three modes. Each enforcer declares the modes it enforces
(`SandboxEnforcer::network_support`), and a policy whose mode it cannot enforce is refused with
`SandboxError::Unenforceable` — before anything starts, never run with more network access
than it asks for.

| Mode | macOS Seatbelt | Linux bubblewrap | Windows restricted token |
|------|----------------|------------------|--------------------------|
| `Denied` | `(deny network*)`, Unix-domain sockets included | `--unshare-net`: no IP traffic and no abstract Unix sockets; see below for sockets on the filesystem | Refused, unless the enforcer is built with `acknowledge_open_network()`; then the network stays open and the first such policy in the process logs a `tracing::warn` |
| `Proxied { endpoint }` | Only `network-outbound` to `localhost:<port>` of the proxy | `--unshare-net`, with the proxy's listener placed on `127.0.0.1:<port>` inside the sandbox's network namespace (feature `egress-proxy`; refused without it) | Refused the same way as `Denied` |
| `Open` | `(allow network*)` with DNS and trust services | Host network namespace | Open |

Start the proxy of a call with `EgressProxy::builder(policy).start_for(enforcer)` (feature
`egress-proxy`; see [Network Egress Policy](../security/egress.md#egress-proxy-for-sandboxed-processes))
and build the call's policy with the `CallProxy`'s `network()`; the proxy listens where the
enforcer needs it (`SandboxEnforcer::proxy_listener`):

| Enforcer | Where the proxy listens |
|----------|-------------------------|
| Seatbelt | The host's loopback, before the command starts |
| bubblewrap | The sandbox's own loopback: bubblewrap reports the sandbox's process id (`--info-fd`) and holds the program (`--block-fd`) while a short-lived helper joins the sandbox's user and network namespaces, brings up its loopback interface, opens the listening socket and passes it back; the program is released once the proxy serves it, or with no listener if a step fails |

A `Proxied` process runs with `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` (upper and lower
case) set to `http://127.0.0.1:<port>` and `NO_PROXY` empty, applied after the policy's and the
request's environment, so neither routes around the proxy. The proxy decides every
destination; a program that ignores the variables reaches nothing.

**Unix sockets under bubblewrap.** A socket on the filesystem is reached through its path, not
the network namespace, and a read-only bind does not refuse `connect`. Under a `Denied` network:

| Policy | Effect |
|--------|--------|
| A grant of `/run`, or of a path under it, that holds a socket or cannot be fully listed | Refused with `Unenforceable` |
| A grant of `/tmp`, `/var/tmp` or `/dev/shm` itself | Refused; a directory below them can be granted |
| A grant above them, such as `/` | Each is covered with an empty, read-only `tmpfs` before deeper grants are mounted, hiding the sockets of Docker, D-Bus, systemd, tmux, X11 and SSH agents |

A socket elsewhere under a granted path, such as in a home directory, stays reachable; grant
directories that hold none. Seatbelt refuses Unix-socket connections under `Denied` and
`Proxied` itself.

Policies serialized before 3.0 load: `allowNetwork: true` becomes `Open`, `false` becomes
`Denied`, and `networkRules`, which no enforcer ever applied, is dropped.

**Windows (restricted token):** the first Windows tier (spec 3.2 §10 W5). `WindowsEnforcer::run_confined` starts the command with `CreateProcessAsUserW` under a restricted, Low-integrity token and holds it in a Job Object; AppContainer is not used.

| Aspect | Behaviour |
|--------|-----------|
| Token | Privileges removed (`DISABLE_MAX_PRIVILEGE`); every group deny-only, `Administrators` included, except `Everyone`, `BUILTIN\Users`, Authenticated Users, `INTERACTIVE`, the logon SID and integrity labels, which the shell needs to start; restricting SIDs `Everyone`, `BUILTIN\Users`, `RESTRICTED`, the logon SID and the user; integrity level Low |
| Paths | `RESTRICTED` is granted on each policy path — full access for read-write, read and execute for read-only — added to the path's existing ACL; each read-write path is labelled Low integrity (no write up), inherited by what lies beneath it |
| Writes | Refused outside the read-write paths: an unlabelled object is Medium integrity, which a Low token cannot write. Paths already labelled Low, such as `%USERPROFILE%\AppData\LocalLow`, stay writable |
| Reads | Allowed where the user may read, the user's profile included; refused where only groups the token holds deny-only, such as `Administrators`, are granted |
| Named objects and pipes | Created in the logon session's namespace, which grants the logon SID and admits Low integrity |
| MSYS2 programs | A program beside `msys-2.0.dll` (Git for Windows' `usr\bin\sh.exe`), or a Git for Windows launcher of one (`bin\sh.exe`, which also sets `MSYSTEM` and puts its `bin` directories first on `PATH`), runs from a private copy of the runtime: `msys-2.0.dll`, the program, `sh.exe`, `bash.exe` and `env.exe`, copied for the call into an owner-only, Medium-integrity directory under `%LOCALAPPDATA%\adk-sandbox\msys2`, compared with the installation's files before use, first on `PATH`, and removed when the call's processes end. Its `etc\fstab` roots it at the installation. The call's MSYS2 processes therefore share an object namespace — the runtime's shared memory and mount table — with no other process. A policy path that holds or lies inside that directory is refused |
| The user's MSYS2 namespaces | Held for the call's life, at the caller's integrity level, by an unconfined `cat` of each installation the call can reach — the program's and those on the call's `PATH` — started before the call, so the Low token can neither write nor create them. An MSYS2 program that a program outside MSYS2 starts itself, such as a hook's `sh` that `git` starts, cannot start, and exits with `NtCreateDirectoryObject(...): 0xC0000022` |
| Other MSYS2 or Cygwin installations | **Not isolated** — a call that starts such an installation's program by its path, while none of the user's runs, creates that installation's namespace at Low integrity, and a program of it the user starts before the call ends joins it and reads a mount table the call wrote. A job released with `kill_on_close(false)` keeps the call's holds until the caller's process exits |
| Descendants | Killed with the call's Job Object, also on a timeout |
| Network | **Not enforced** — Windows has no per-process network control without AppContainer. A policy whose network is `Denied` or `Proxied` is refused with `SandboxError::Unenforceable` unless the enforcer is built with `WindowsEnforcer::acknowledge_open_network()`; then the command keeps full network access and the first such policy in the process logs a `tracing::warn` |
| After the call | The grants and the Low labels stay on the policy's paths, so any Low-integrity process of the machine may write the read-write paths; `icacls <path> /setintegritylevel "(OI)(CI)M" /T` and `icacls <path> /remove:g *S-1-5-12 /T` take a path back |

The user is a restricting SID because the MSYS2 runtime of Git for Windows' `sh` and `bash` creates its signal pipe and its per-user shared memory with an ACL naming only the user, `Administrators` and `SYSTEM`, then opens them again: without the user among the restricting SIDs, the shell exits at start with `couldn't create signal pipe, Win32 error 5`. That widens reads from the paths the policy grants (and those granting `Everyone` or `Users`) to every path the user may read; the Low integrity level keeps writes to the read-write paths, and narrows them where a path's ACL grants `Everyone` or `Users` write access, which a Medium label still closes.

A restricted token is set when the process is created, so it cannot be attached to a `tokio::process::Command` that the caller spawns. `probe()` therefore returns `EnforcerUnavailable` and `configure_command()` returns `EnforcerFailed`, naming `run_confined`: `sandboxed_command` and `ProcessBackend::with_sandbox` do not confine on Windows and report so. The enforcer starts a command itself in two ways:

| Call | Returns |
|------|---------|
| `run_confined(policy, program, args, cwd, timeout)` | Once the command exits or times out, its exit code; standard streams are not captured |
| `spawn_contained(policy, program, args, cwd)` | At once, a `process_tree::ContainedChild`: the running process (`wait`, `try_wait`), its Job Object, and its standard input, output and error as pipes; it inherits those three handles and no other |

The three pipe ends `spawn_contained` hands the process are never inheritable in the calling process, so a process another thread starts meanwhile — through `std::process::Command`, which passes on every inheritable handle — cannot take one and hold the command's output open after the command exits. A handle list (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`) accepts only inheritable handles, so their inheritable copies are made in a broker instead: `cmd.exe` from the system directory, created suspended in the call's Job Object and never run, which the process names as its parent (`PROC_THREAD_ATTRIBUTE_PARENT_PROCESS`) and inherits the three handles from. The broker ends once the process exists, so the process's parent process id is that of an exited process, and it shares the caller's console as before. The unconfined `cat` that holds an MSYS2 namespace starts the same way.
| `probe_confined()` | `Ok` when `cmd.exe /d /c exit 0` runs under the restricted token, the probe of these two calls |

The harness shell runs `bash` through `spawn_contained` on Windows (spec 3.2 §10 W5; see [Harness Shell and Sandbox](../harness/shell-and-sandbox.md)). The PR tier's `windows` job runs the enforcer's tests.

### Process trees on Windows

A Windows process has no process group, so ending a process leaves what it started
running. `adk_sandbox::process_tree::JobObject` (feature `process-tree`, implied by
`sandbox-windows`; Windows only) holds a process tree in a Job Object that ends its
processes when its last handle closes (spec 3.2 §10 W1). Start the process with
`CREATE_SUSPENDED` and pass its id to `JobObject::contain_suspended`, which places it in a
new job and only then lets it run, so nothing it starts is outside the job.

| Call | Does |
|------|------|
| `JobObject::contain_suspended(pid)` | Places the suspended process in a new job, then resumes it |
| `JobObject::new()`, `assign(handle)` | An empty job, and a process added to it |
| `terminate()` | Ends every process in the job |
| `kill_on_close(false)` | Lets the processes run on once the job's last handle closes |
| Dropping the job | Ends every process left in it, also when the owning process exits |

```rust,ignore
use adk_sandbox::process_tree::{CREATE_SUSPENDED, JobObject};

async fn run() -> std::io::Result<()> {
    let mut command = tokio::process::Command::new("cmd.exe");
    command.args(["/c", "start /b ping -n 100 127.0.0.1"]).creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn()?;
    let job = JobObject::contain_suspended(child.id().expect("the child runs"))?;
    child.wait().await?;
    // `ping`, which the shell left running, ends with the job.
    drop(job);
    Ok(())
}
```

The devtools `bash` tool, each stdio server of `adk-tool`'s `McpServerManager` (and so each
kernel driver), and `WindowsEnforcer::run_confined` hold their processes this way.

### Process trees on macOS and Linux

On Unix, `adk_sandbox::process_tree::ProcessGroup` (feature `process-tree`) holds a process
tree in a session and process group of its own (spec 3.2 §7.7, K-I5). Start the command with
`ProcessGroup::new_session`, which makes the process the leader of a new session with no
controlling terminal, and hand the spawned child to `ProcessGroup::contain`. The group keeps
the leader unreaped until it ends the group, so the group's id cannot name another group
while it is signalled.

| Call | Does |
|------|------|
| `ProcessGroup::new_session(&mut command)` | Starts the command's process as the leader of a new session and process group |
| `ProcessGroup::contain(child)` | Holds the spawned leader; refuses, and kills, a child that leads no group of its own |
| `wait_for_leader(limit)` | Waits for the leader to exit, without reaping it |
| `terminate()` | Sends the group `SIGKILL` |
| `terminate_and_wait(limit)`, `terminate_and_wait_async(limit)` | Sends the group `SIGTERM`, then `SIGKILL` once the leader has exited or half of `limit` has passed, reaps the leader, and returns whether every process of the group has exited within `limit` |
| Dropping the group | Sends `SIGKILL` to whatever is left in it |

```rust,ignore
use adk_sandbox::process_tree::ProcessGroup;

async fn run() -> std::io::Result<()> {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", "sleep 100 & sleep 100"]);
    ProcessGroup::new_session(&mut command);
    let group = ProcessGroup::contain(command.spawn()?)?;
    // The `sleep` the shell left in the background has exited too when this returns.
    assert!(group.terminate_and_wait_async(ProcessGroup::TERMINATION_WAIT).await?);
    Ok(())
}
```

A process group is not a boundary the way a Job Object is: a process that calls `setsid` or
`setpgid` leaves it, and nothing ends the group when its owner is killed outright. Each stdio
server of `adk-tool`'s `McpServerManager`, and so each kernel driver, is held this way.

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
