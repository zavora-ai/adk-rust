# Harness Shell and Sandbox

The `HarnessAgent` (`adk-harness`) runs model-written shell commands through one `bash`
tool. By default the command runs on the host behind a per-command approval; the OS sandbox
and the container shell are opt-in, and a strict deployment can refuse to start without
them. This page covers the sandboxed and container shells: what they can reach, how a failed
command is retried outside them, and what the harness records.

## Isolation

| `ShellIsolation` | Where `bash` runs | Reported `IsolationStatus` |
|---|---|---|
| `Host` (default) | `sh -c` on the host, with the user's permissions and network | `Host` |
| `OsSandbox(OsSandboxConfig)` | `sh -c` under Seatbelt (macOS), bubblewrap (Linux) or a restricted token (Windows) | `OsSandbox { enforcer }` |
| `OsSandbox(..)` with no working enforcer | the host, as `Host` | `Degraded { requested, reason }` |
| `Container(ContainerConfig)` (feature `docker`) | `sh -c` in a container per session, the workspace mounted at `/workspace` | `Container { image }` |
| `Container(..)` with no reachable Docker daemon | the host, as `Host` | `Degraded { requested: "container", reason }` |

The enforcer — for a container, the Docker daemon — is probed when the harness is built,
and the sandbox again on every call:

| Situation | Result |
|---|---|
| Enforcer unavailable at build, `fail_if_unavailable` set | `build()` fails with `harness.sandbox_unavailable` |
| Enforcer unavailable at build, no `fail_if_unavailable` | A warning; the shell is `Degraded`: it declares and behaves as the host shell, every command needs approval in Ask, and `auto_allow_sandboxed` has no effect |
| Enforcer available at build, unavailable at a call | The call is refused with `harness.sandbox_unavailable` and a `harness.sandbox.unavailable` record; it never falls back to the host |
| A session's container cannot be provisioned or started, or lets a protected path be written | The same refusal, with the reason in the record |

On Windows the sandbox is the `restricted-token` enforcer (spec 3.2 §10 W5). A restricted
token is set when a process is created, so the enforcer starts each command itself, through
`WindowsEnforcer::spawn_contained`, and the `bash` tool reads its output from pipes. Its probe
(`WindowsEnforcer::probe_confined`) runs `cmd.exe /d /c exit 0` under the token: the shell is
`OsSandbox { enforcer: "restricted-token" }` when it succeeds, and `Degraded` with the reason,
or refused under `fail_if_unavailable`, when it fails. See
[The Windows sandbox](#the-windows-sandbox) for what it confines.

## Turning the sandbox on

```rust,no_run
use std::sync::Arc;

use adk_harness::{HarnessAgent, HarnessMode, IsolationStatus, OsSandboxConfig, ShellIsolation};
use adk_model::gemini::GeminiModel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Ask)
        .shell(ShellIsolation::OsSandbox(OsSandboxConfig::default()))
        .fail_if_unavailable()
        .build()?;
    assert!(matches!(harness.info().isolation, IsolationStatus::OsSandbox { .. }));
    Ok(())
}
```

`OsSandboxConfig` fields:

| Field | Default | Meaning |
|---|---|---|
| `network` | `false` | Allow all network access; `adk-sandbox` cannot filter by domain |
| `allow_process_spawn` | `true` | Allow forking; `sh -c` forks for every external command |
| `extra_read`, `extra_write` | empty | Additional read-only and read-write paths |
| `read_toolchains` | `true` | Read-only access to the toolchain directories below |
| `enforcer` | `None` | The enforcer; `None` uses `adk_sandbox::sandbox::get_enforcer()` |

## What a sandboxed command can reach

`SandboxLauncher` builds each command with `adk_sandbox::sandbox::sandboxed_command` as the
launcher of the devtools `BashTool`, which keeps its timeout ceiling, output cap, streaming,
empty standard input, new session and process-group kill.

| | Ask, Auto (read-write) | Plan, ReadOnly (read-only) |
|---|---|---|
| Workspace root | read-write | read-only |
| Protected paths that exist (`.git`, `.adk`, `.skills`, `.claude`, `.cargo`, `AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `MEMORY.md`, `.envrc`, `lefthook.yml`, `lefthook.yaml`, and the same names up to four levels below the root) | read-only | read-only |
| Per-session scratch directory (`HOME`, `TMPDIR`) | read-write | read-write |
| Toolchains (`read_toolchains`) | read-only | read-only |
| System runtime (`/usr`, `/bin`, `/System`, …, as the enforcer grants it) | read-only | read-only |
| Anything else, including the real home directory and `~/.ssh` | none | none |
| Network | `network` | `network` |

**Git.** Of a `.git` directory the sandbox keeps `hooks`, `config` and `info` read-only, the
entries that decide which commands git runs, and leaves the index, refs and objects
writable, so `git add` and `git commit` work inside it. The same holds for a nested
repository's `.git` and for each submodule gitdir under `.git/modules`.

| `.git` | Kept read-only |
|---|---|
| A directory with `hooks`, `config` and `info`, none a symbolic link | Those three entries |
| A directory missing one of them, or with one a symbolic link | The whole directory; `git commit` needs the unsandboxed retry |
| A file naming the gitdir (a linked worktree or a submodule checkout) | The file. The gitdir lies outside the workspace, so `git commit` needs the unsandboxed retry |

The file tools keep all of `.git` protected, and rewriting history is reversible through the
reflog.

**Toolchains.** With `read_toolchains`, the sandbox can read the `PATH` directories, the
Cargo, rustup, pyenv and nvm homes, `/opt/homebrew` and `/usr/local`, and on Linux `/usr`,
`/bin`, `/lib`, `/lib64` and `/etc`. A `PATH` entry that is the home directory, a directory
above it, or a directory that holds the workspace (a projects directory on `PATH`) is left
out, since it would expose other projects' files.

| Platform | Extra handling |
|---|---|
| macOS | The active developer directory (`xcode-select -p`) is readable, `DEVELOPER_DIR` and `SDKROOT` are set, and its compiler and `git` directories come first on `PATH`. The `/usr/bin` shims run `xcrun`, which cannot run under Seatbelt; the tools behind them can, so `cargo test` links |
| Linux | bubblewrap starts from an empty root. A toolchain path that is a symbolic link — `/bin`, `/lib`, `/lib64` on a merged-`/usr` distribution — is recreated as a link (`--symlink`) and its target bound, so `/bin/sh` and the dynamic loader resolve; `/dev` and `/proc` are mounted |

**Environment.** The workspace's `bash` allowlist (`Workspace::env_allowlist`, default
`PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`, `TERM`, `USER`, `SHELL`), with `HOME` and
`TMPDIR` set to the scratch directory. Agent sockets (`SSH_AUTH_SOCK`, `SSH_AGENT_PID`,
`GPG_AGENT_INFO`) and variables named like credentials (`TOKEN`, `SECRET`, `PASSWORD`,
`PASSWD`, `API_KEY`, `CREDENTIAL`) never pass in, whatever the allowlist says. With
`read_toolchains`, `CARGO_HOME`, `RUSTUP_HOME`, `PYENV_ROOT` and `NVM_DIR` point at the real
toolchain directories, since `HOME` moved.

## The Windows sandbox

The restricted-token enforcer runs `sh -c <script>` — the `sh` the sandbox's `PATH` finds,
from Git for Windows, as the host shell does — under a Low-integrity token whose groups are
deny-only (see the [sandbox reference](../sandbox/index.md)). The workspace and the scratch
directory are labelled Low, so the command writes there and nowhere else. The command starts in a Job Object, so the `bash`
tool keeps its timeout ceiling, output cap, streaming, empty standard input, the kill of
every process the command starts, and the freeze's kill (spec 3.2 §10 W1). It inherits its
three standard handles and no other handle of the agent, and none of the three is ever
inheritable in the agent's process, so a process the agent starts meanwhile — an MCP server,
a hook — cannot hold the command's output open after it exits.

| | Under the restricted token |
|---|---|
| Workspace root, scratch directory, `extra_write` | read-write: `RESTRICTED` is granted on each and each is labelled Low, once per process; the grant and the label stay on the path |
| Protected paths | **writable**: a path inside the workspace inherits its read-write grant and its Low label |
| Toolchains (`read_toolchains`) | read-only when they lie in your profile (`~/.cargo`, `~/.rustup`, …); program and system directories grant `Users` read access already and are not rewritten |
| Your profile and anything else you may read | **readable**: MSYS2, the runtime of Git for Windows' `sh`, opens its own signal pipe and shared memory through an ACL that names only you, so you are one of the token's restricting SIDs |
| Anything else you may write, including paths that grant `Everyone` or `Users` write access | none: an unlabelled path is Medium integrity, which the Low token cannot write; paths already labelled Low, such as `%USERPROFILE%\AppData\LocalLow`, stay writable |
| Network | **not restricted**, whatever `network` says |
| Environment | as on the other platforms, with `TEMP` and `TMP` also set to the scratch directory; Git for Windows' launcher settings (`MSYSTEM`, its `bin` directories first on `PATH`) when `sh` is Git's `bin\sh.exe` |
| MSYS2, the runtime of `sh` and of the programs it starts | a private copy for each command: `msys-2.0.dll`, `sh.exe`, `bash.exe` and `env.exe`, copied into an owner-only, Medium-integrity directory under `%LOCALAPPDATA%\adk-sandbox\msys2`, compared with Git's files before use, first on `PATH`, and removed when the command's processes end; the command's MSYS2 processes share their object namespace, the mount table among it, with no other process |
| Your own Git Bash and other MSYS2 programs of an installation on `PATH` or of `sh`'s | **kept apart**: the command holds your MSYS2 namespace of each such installation at your integrity level with an unconfined `cat` of it, started before the command, so the command can neither write it nor create it for your next Git Bash to join. An MSYS2 program that a program outside MSYS2 starts itself cannot start: `git` starts a hook's `sh` from its own installation, so a commit with a hook fails, with a hint that suggests an approved unsandboxed retry |
| MSYS2 or Cygwin installations neither on `PATH` nor `sh`'s | **not isolated**: a command that starts such an installation's program by its path, while none of yours runs, creates that installation's namespace at Low integrity, and a program of it you start before the command ends joins it |

What follows from that:

| Behaviour | Windows | macOS, Linux |
|---|---|---|
| The `bash` declaration's `network` annotation and description | network access | `network` |
| The `bash` declaration's `reads` annotation, `IsolationStatus::confines_reads` and the status line | `unconfined`, `false`, `OS sandbox (restricted-token, network not restricted, reads not confined, MSYS2 installations off PATH not isolated)`; the declaration digest differs from a confining sandbox's, so a standing rule granted under Seatbelt or bubblewrap does not match it | `confined`, `true` |
| The `bash` declaration's `msys2Isolated` annotation, `harness.bash.begin`'s `msys2Isolated` and `IsolationStatus::isolates_msys2` | `false`: MSYS2 installations neither on `PATH` nor `sh`'s are not isolated | `true` |
| Plan mode | `bash` is denied: a read-write grant from an earlier command stays in the workspace's ACL, so the sandbox cannot promise a read-only workspace | sandboxed commands run read-only without a prompt |
| Ask and Auto mode | every sandboxed command is asked | as the mode table says |
| `auto_allow_sandboxed` | `build()` fails: the sandbox does not restrict the network, does not confine reads to the workspace and does not isolate the MSYS2 installations off its `PATH` | needs every protected path |
| Hooks with `isolation = "inherit"` | `build()` fails: a hook needs a command it can spawn and feed; give it `isolation = "host"` | run in the sandbox |

The first command of a process grants the workspace, and with `read_toolchains` the
toolchain directories in your profile; Windows applies a grant to everything beneath the
path, which takes a while on a large tree. Later commands reuse the grants.

**Labels and grants that stay.** The token writes only what is labelled Low integrity, so
the sandbox labels each read-write path and grants it to `RESTRICTED`:

| Path | Labelled Low | Afterwards |
|---|---|---|
| The workspace root, and what is created beneath it | from the first sandboxed command on; the label stays | any Low-integrity process of the machine may write the workspace |
| Each `extra_write` path | the same | the same |
| The session's scratch directory (`HOME`, `TMPDIR`, `TEMP`, `TMP`) | while the harness runs | removed with its label when the harness is dropped; a killed process leaves it in the temporary directory |
| Read-only paths (toolchains, `extra_read`) | never; they keep `RESTRICTED`'s read entry | — |

To take a workspace back, restore its Medium label and remove the grant:

```powershell
icacls "C:\path\to\workspace" /setintegritylevel "(OI)(CI)M" /T
icacls "C:\path\to\workspace" /remove:g *S-1-5-12 /T
```

## The container shell

With feature `docker`, `ShellIsolation::Container` runs each session's commands in its own
container. The host workspace is bind-mounted at `/workspace`, so a file `bash` writes there
is the file `read_file` reads, and every protected path that exists is mounted read-only over
it.

```rust,no_run
use std::sync::Arc;

use adk_harness::{ContainerConfig, HarnessAgent, HarnessMode, IsolationStatus, ShellIsolation};
use adk_model::gemini::GeminiModel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Ask)
        .shell(ShellIsolation::Container(ContainerConfig {
            image: "rust:1.95-slim".to_string(),
            ..ContainerConfig::default()
        }))
        .fail_if_unavailable()
        .build()?;
    assert!(matches!(harness.info().isolation, IsolationStatus::Container { .. }));
    Ok(())
}
```

`ContainerConfig` fields:

| Field | Default | Meaning |
|---|---|---|
| `image` | `ubuntu:22.04` (`DEFAULT_CONTAINER_IMAGE`) | The image of every session's container; it must be present on the daemon |
| `network` | `false` | Docker's `bridge` network; `false` leaves only loopback |
| `extra_read`, `extra_write` | empty | Extra host paths mounted read-only or read-write, as (host path, container path) |
| `user` | `None` | The user commands run as; `None` is the current `uid:gid`, so files the container writes into the workspace belong to you |
| `memory_limit_bytes`, `cpu_limit` | `None` | Container resource limits |
| `idle_ttl` | 15 minutes | How long a session's container stays up after its last command |
| `client` | `None` | A `SandboxClient` of your own; it must mount the workspace at its root itself and run as a user that may write it (`DockerClient::with_user`) |

| Lifecycle | Behaviour |
|---|---|
| First command of a session | `SandboxClient::provision` and `start` create the session's container; a probe then tries to write each protected path inside it and refuses the container if one write succeeds |
| Later commands, later runs | The same container, one per (app, user, session) |
| Idle for `idle_ttl` | The container is stopped; the next command provisions a new one |
| A command times out, or a freeze stops it | The container is stopped, which ends the command and everything it started |
| `Harness::close` | Every session's container is stopped |

A command runs through `SandboxSession::exec_command` with the workspace's timeout ceiling,
and its output is cut as [Output](#output) describes once it finishes. Commands in
one session's container run one at a time; when one exits, every process it left in the
container is killed. The container drops every capability, sets `no-new-privileges` and
limits processes to 512.

| Differs from the OS sandbox | Container shell |
|---|---|
| Unsandboxed retry | Off unless `allow_unsandboxed_retry(true)`; when on, it is the same once-only, non-waivable host retry |
| Hooks with `isolation = "inherit"` | Refused at build with `harness.hooks.config`; hooks with `isolation = "host"` run |
| A goal's `until` check | Refused, never run on the host in the container's place; the goal ends `exhausted` |
| Shell credentials (`shell_credential`) | Refused at build |
| Plan mode | Deny: the container has no read-only workspace mount |

## Modes and approvals

| Mode | Sandboxed `bash` | Container `bash` | Unsandboxed retry |
|---|---|---|---|
| ReadOnly | Deny | Deny | Deny |
| Plan | Allow, read-only workspace, under an active enforcer only | Deny | Deny |
| Ask (default) | Approval; Allow with `auto_allow_sandboxed` | Approval; Allow with `auto_allow_sandboxed` | Approval, non-waivable |
| Auto | Allow when sandboxed commands may run unapproved (below); Approval otherwise | Allow | Approval, non-waivable |

The `bash` declaration's annotations are `{enforcer, profileDigest, network}`, where
`profileDigest` covers the enforcer, network, process spawning, toolchain reads and extra
paths. A standing rule or a held call is bound to the declaration digest, so a rule granted
under one sandbox configuration never applies under another, nor on the host.

### `auto_allow_sandboxed`

Sandboxed commands run without approval in Ask (`auto_allow_sandboxed`), and in Auto, only
when every condition holds:

| Condition | Checked |
|---|---|
| An OS sandbox or a container shell is active (not `Host`, not `Degraded`) | At build; refused per call if it goes away |
| `network` is `false` | At build |
| The enforcer keeps a read-only path inside a read-write path read-only: a conformance script tries to write, create, remove, rename and link beneath one. For a container, each protected path is mounted read-only and a probe in the container tries to write it | At build; for a container, when its container starts |
| Every root-level protected path exists and is not a symbolic link | At build and on every call |

While `auto_allow_sandboxed` is in effect, no host file tool (`read_file`, `glob`, `grep`,
`write_file`, `edit_file`) runs at the same time as a sandboxed or container command, in any
session or team member of the harness: the file tools wait for the command to finish, and a
command waits for running file tools. A command cannot rearrange the workspace under a host
file tool that is resolving a path in it.

A protected path that does not exist cannot be protected from creation, and a symbolic link
can be replaced, so `build()` fails with `harness.auto_allow_sandboxed_unavailable` naming
each such path. Create the missing ones (`.adk`, `.skills`, `.claude` and `.cargo` as
directories, the rest as files) to turn the setting on. If a protected path disappears
later, the next sandboxed command needs approval again.

## A failed command and the unsandboxed retry

When a sandboxed command exits non-zero, its result carries a `sandbox` object:

```json
{
  "exit_code": 1,
  "stderr": "sh: /Users/me/notes.txt: Operation not permitted\n",
  "sandbox": {
    "enforcer": "seatbelt",
    "denialSuspected": true,
    "hint": "The sandbox (seatbelt) may have refused this: writes outside the workspace or to protected paths, and network access, are denied. If the command needs them, retry it once with unsandboxed: true, retry_of: \"call-7\" and a reason; the user must approve the retry."
  }
}
```

`denialSuspected` is set when the output contains `Operation not permitted`,
`Read-only file system`, `deny(1)`, or a name-resolution or unreachable-network error. Every
call is recorded in the per-session shell ledger.

The model may then call `bash` with `unsandboxed: true`, `retry_of` and `reason`. The
policy refuses the retry unless `retry_of` names a failed sandboxed call in the same session
with the identical command and no earlier retry, and refuses it outright in ReadOnly and
Plan or with `allow_unsandboxed_retry(false)`. Otherwise it is held for a non-waivable
approval; the prompt is titled as a host command and shows the model's reason quoted,
escaped and capped. The approved retry runs on the host and consumes the failed call, so a
replay of it is refused without a prompt.

With the retry off — `allow_unsandboxed_retry(false)`, and always under the personal preset
([Personal preset](personal.md)) — `bash` declares no `unsandboxed`, `retry_of` or `reason`,
its hint offers no retry, and a call that sends `unsandboxed: true` is refused with
`harness.bash.retry_refused`. The declaration digest differs, so a standing rule granted with
the retry on does not cover the shell with it off.

## Process lifetime and the kill switch

| Guarantee | Host | Seatbelt | bubblewrap | Container |
|---|---|---|---|---|
| Standard input is `/dev/null` | yes | yes | yes | no standard input |
| New session, no controlling terminal | `setsid` | `setsid` | `setsid` and `--new-session` | no terminal is attached |
| Background processes die when the command exits | process-group kill; on Windows the command's Job Object | process-group kill; on Windows (restricted token) the command's Job Object | the PID namespace ends with `bwrap` | every process but the container's own is killed when the command exits |
| A freeze kills the running command | within 250 ms of the poll | within 250 ms of the poll | within 250 ms of the poll | the container is stopped within 250 ms of the poll |

A process that starts its own session (`setsid`) leaves the process group and survives the
group kill on the host and under Seatbelt; bubblewrap's PID namespace still ends it. On a
Windows host no process the command starts can leave its Job Object.

## Output

A `bash` result carries at most 30,000 characters of output (spec 3.1 §7.33 fix 8); the
whole output of a longer one is kept for `shell_output`.

| Rule | Behaviour |
|---|---|
| Inline | At most `adk_devtools::INLINE_OUTPUT_CHARS` (30,000) characters, stdout and stderr together; a stream within half of it keeps all of it and the other stream takes the rest |
| Cut | A longer stream keeps the first third and the last two thirds of its share — the first 10,000 and the last 20,000 characters when one stream has it all — around a marker naming the bytes left out and the `shell_output` call that reads them |
| Kept | The whole output goes to a file outside the workspace, `adk-harness-output-<uid>/<session>/shells/<id>.log` in the system's temporary directory (`ShellOutputs`), private to the user; the result names it as `output_id` |
| Disk bound | The file holds at most `Workspace::max_output_bytes`, 64 MiB by default (`adk_devtools::DEFAULT_MAX_OUTPUT_BYTES`); memory holds only the start and end of each stream |
| No cut | An output that fits inline leaves no file |
| Container shell | The same cut and file, applied once the command finishes |

```json
{
  "command": "cargo test",
  "exit_code": 101,
  "stdout": "running 812 tests\n…[5212880 bytes left out; shell_output {\"shell_id\": \"1\"} reads the whole output]\ntest result: FAILED. 811 passed; 1 failed\n",
  "stderr": "",
  "truncated": true,
  "output_id": "1"
}
```

`shell_output { shell_id, tail_lines?, offset?, limit?, pattern? }` (class `Read`) reads at
most 30,000 characters of a kept output: its end by default, `limit` bytes from `offset`, the
last `tail_lines` lines, or the lines matching the regular expression `pattern`, numbered. It
reads only the calling session's files and never follows a link in their place.

## Background shells

`bash` with `run_in_background: true` starts the command and returns at once (spec 3.1
§7.29). A dev server, a watcher or a long build keeps running while the agent works.

| Tool | Arguments | Class | Result |
|---|---|---|---|
| `bash` | `command`, `run_in_background`, `description?` (a few words shown to the person) | The class of the same command in the foreground | `{ shell_id, status: "running", command, note }`; the note tells the model it is told when the shell exits and must not poll |
| `shell_output` | `shell_id`, `tail_lines?`, `offset?`, `limit?`, `pattern?` | `Read` | `{ shell_id, status, exit_code?, killed_by?, duration_ms, command, total_bytes, output, … }` |
| `shell_kill` | `shell_id` | `Read`: allowed in every mode, since it only removes an effect | `{ shell_id, status, killed, duration_ms, exit_code? }` |

| Rule | Behaviour |
|---|---|
| Decision | Exactly the foreground decision: the same class, the same `ShellPrefix` rules and the same isolation digest; a prompt's title ends with `, in the background` |
| Declaration | `run_in_background` and `description` join the declaration, so a standing rule granted before them is stale once (`harness.rule.stale`) |
| Spawn | The same launcher, environment and empty standard input as a foreground call; the output goes to the shell's file from its first byte |
| Duplicates | A command identical to one still running in the session returns that shell, with `already_running: true` |
| Foreground timeout | A call that reaches its timeout moves to the background under the same decision instead of being killed; the result carries `moved_to_background: true`, `timeout_secs` and the `shell_id`. A session already running its most shells kills it as before |
| Completion | A shell that ends is named in the reminder of every request until `shell_output` reads it or `shell_kill` removes it: `shell 3 ("cargo watch") exited 101 after 4m12s; last line: "error[E0308]: mismatched types"; shell_output 3`. The line is data: quoted, cut and with control characters escaped; the policy never reads it |
| Sessions | A session lists, reads and stops only its own shells |
| Shell ledger | A background command is recorded when it ends, with its exit code, so a failed sandboxed one can be retried unsandboxed |
| Action ledger | The call's result is the started shell, so a re-issued call is answered from its record and never starts a second server |
| People | Output streams to the console through `emit_progress` as a foreground call's does; `/shells` lists each shell with its id, state, age and command, and `/shells kill N` stops one |

`BackgroundShellConfig` sets the limits:

| Limit | `unattended()` (the default) | `attended()` (the adk-rust REPL) |
|---|---|---|
| Running shells per session (`max_running`) | 8 | 8 |
| Output that kills a shell (`max_output_bytes`) | 64 MiB | 64 MiB |
| Lifetime (`with_lifetime`, at most 2 h) | 30 minutes | none |

```rust,no_run
use std::sync::Arc;
use std::time::Duration;

use adk_harness::{BackgroundShellConfig, HarnessAgent, HarnessMode};
use adk_model::gemini::GeminiModel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let harness = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Ask)
        .background_shells(
            BackgroundShellConfig::unattended().with_lifetime(Duration::from_secs(2 * 60 * 60)),
        )
        .build()?;
    assert!(harness.runtime().toolset().tool_names().iter().any(|name| name == "shell_kill"));
    Ok(())
}
```

| Shell ends | How |
|---|---|
| It exits | Its exit code and last line are recorded; the process group is killed as a foreground call's is |
| A freeze | The shell's watcher polls the run's kill switch every 250 ms and kills it |
| Its output or lifetime limit | The watcher kills it and records why (`killed_by`: `output_limit`, `lifetime`) |
| `shell_kill`, `/shells kill N` | Killed and removed from the reminder |
| Session end | `Harness::end_session_shells` kills the session's shells |
| `Harness::close`, the process exiting | Every shell is killed; the adk-rust REPL closes the harness when it exits |
| A crash | Each shell's pid and start time, and the harness process's, are kept in a `ShellPidRegistry` (`harness.db` in adk-rust); the next start kills the process group of every shell whose harness process is gone, unless its pid now names a process started later |

| Isolation | What background shells suit |
|---|---|
| Host | Servers, watchers and long builds, with the same approval as the command in the foreground |
| OS sandbox | Builds, tests and watchers. bubblewrap unshares the network per call, so a server started there is unreachable from later commands, and Seatbelt binds no port without network: a server needs the host, through an approved unsandboxed retry |
| OS sandbox with `auto_allow_sandboxed` | None: a sandboxed command in the background is refused with `harness.shell.background_gated`, since no host file tool may run beside an unapproved sandboxed command (I26), and a timed-out one is killed |
| Container shell | None yet: it declares no `run_in_background`, and a timed-out command is killed |

## Audit records

| Action | Fields (`ext`) |
|---|---|
| `harness.bash.begin` (write-ahead, `args_digest`, `payload_ref` = digest of the command) | `functionCallId`, `enforcer`, `profileDigest`, `network`, `reads`, `workspaceAccess`, `envKeys`, `retryOf`, `goalCheck`, `background` |
| `harness.bash.end` (`outcome`, `payload_ref` = digest of the result) | `functionCallId`, `enforcer`, `exitCode`, `durationMs`, `timedOut`, `killedByFreeze`, `truncated`, `denialSuspected`, `retryOf`, `shellId` (a call that started a background shell) |
| `harness.shell.started` | `functionCallId`, `shellId`, `pid` |
| `harness.shell.ended` | `functionCallId`, `shellId`, `exitCode`, `durationMs`, `killedBy` (`requested`, `freeze`, `output_limit`, `lifetime`, `session_end`, `close`), `truncated` |
| `harness.bash.retry_unsandboxed` | `functionCallId`, `retryOf`, `failedEnforcer`, `failureDigest`, `decision`, `source` (`policy` when requested, `tool` when consumed) |
| `harness.sandbox.unavailable` | `functionCallId`, `requested`, `error` |

## Residual risks

| Risk | Status |
|---|---|
| A process that calls `setsid` outlives the call on the host and under Seatbelt | Documented; bubblewrap ends it with the PID namespace |
| A background shell's group is killed after a crash only at the next start, and only where the OS reports process start times (macOS, Linux) | Documented; on Windows the Job Object ends the shell with the harness process |
| A protected name that does not exist below the root (a new nested `AGENTS.md`, `.envrc` or `.git`) can be created by a sandboxed command | Root-level names gate `auto_allow_sandboxed`; nested files are listed, never inlined, by the instruction loader |
| On a case-sensitive file system a differently cased protected name (`agents.md`) is a different file | File tools match protected names case-folded; the sandbox protects names that exist |
| Network access is all or nothing | `network: false` is required for unapproved sandboxed commands |
| Git runs code from places the sandbox leaves writable: a `core.hooksPath` inside the workspace (Husky's `.husky`), or a repository a command creates and registers as a submodule through the writable index, whose own `config` git honours when it recurses into it | Documented; the sandbox protects the gitdirs that exist when the command starts |
| A protected name created inside the container after it started is not mounted read-only | The container's protected mounts are fixed when it starts; root-level names must exist for `auto_allow_sandboxed`, and the next container mounts new ones |
| A container from a `client` of your own keeps a background process a command leaves, since the harness cannot tell that its sessions are isolated from the host | Containers the harness builds end them after each command; stop them in your client's `exec_command` |
| A sandboxed command can read the workspace's secret files (`.env`) | `grep` and `glob` skip them and `read_file` refuses them; `bash` needs approval unless `auto_allow_sandboxed` is on |

## Red-team scenarios

[`examples/harness_redteam`](https://github.com/zavora-ai/adk-rust/tree/main/examples/harness_redteam)
runs a scripted attacker against the 3.0 `CodingAgent`, the harness with its sandbox
unavailable, and the harness under the platform enforcer, and prints one row per scenario:
poisoned nested `AGENTS.md`, project configuration granting `bash`, `.mcp.json` spawn,
git-hook persistence, background survivors, retry replay, digest drift, a lying
`readOnlyHint`, control characters hiding a command, an `~/.ssh` read, a network canary and a
`.env` grep canary. `cargo run -- --live` runs a sandboxed `cargo test`, a denied write with
its hint and approved retry, and the degraded path on OpenAI, Anthropic and Gemini.
