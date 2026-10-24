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
three standard handles and no other handle of the agent.

| | Under the restricted token |
|---|---|
| Workspace root, scratch directory, `extra_write` | read-write: `RESTRICTED` is granted on each and each is labelled Low, once per process; the grant and the label stay on the path |
| Protected paths | **writable**: a path inside the workspace inherits its read-write grant and its Low label |
| Toolchains (`read_toolchains`) | read-only when they lie in your profile (`~/.cargo`, `~/.rustup`, …); program and system directories grant `Users` read access already and are not rewritten |
| Your profile and anything else you may read | **readable**: MSYS2, the runtime of Git for Windows' `sh`, opens its own signal pipe and shared memory through an ACL that names only you, so you are one of the token's restricting SIDs |
| Anything else you may write, including paths that grant `Everyone` or `Users` write access | none: an unlabelled path is Medium integrity, which the Low token cannot write; paths already labelled Low, such as `%USERPROFILE%\AppData\LocalLow`, stay writable |
| Network | **not restricted**, whatever `network` says |
| Environment | as on the other platforms, with `TEMP` and `TMP` also set to the scratch directory |

What follows from that:

| Behaviour | Windows | macOS, Linux |
|---|---|---|
| The `bash` declaration's `network` annotation and description | network access | `network` |
| The `bash` declaration's `reads` annotation, `IsolationStatus::confines_reads` and the status line | `unconfined`, `false`, `OS sandbox (restricted-token, network not restricted, reads not confined)`; the declaration digest differs from a confining sandbox's, so a standing rule granted under Seatbelt or bubblewrap does not match it | `confined`, `true` |
| Plan mode | `bash` is denied: a read-write grant from an earlier command stays in the workspace's ACL, so the sandbox cannot promise a read-only workspace | sandboxed commands run read-only without a prompt |
| Ask and Auto mode | every sandboxed command is asked | as the mode table says |
| `auto_allow_sandboxed` | `build()` fails: the sandbox does not restrict the network and does not confine reads to the workspace | needs every protected path |
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
and its output is capped at the workspace's `max_output_bytes` after it finishes. Commands in
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

## Audit records

| Action | Fields (`ext`) |
|---|---|
| `harness.bash.begin` (write-ahead, `args_digest`, `payload_ref` = digest of the command) | `functionCallId`, `enforcer`, `profileDigest`, `network`, `reads`, `workspaceAccess`, `envKeys`, `retryOf`, `goalCheck` |
| `harness.bash.end` (`outcome`, `payload_ref` = digest of the result) | `functionCallId`, `enforcer`, `exitCode`, `durationMs`, `timedOut`, `killedByFreeze`, `truncated`, `denialSuspected`, `retryOf` |
| `harness.bash.retry_unsandboxed` | `functionCallId`, `retryOf`, `failedEnforcer`, `failureDigest`, `decision`, `source` (`policy` when requested, `tool` when consumed) |
| `harness.sandbox.unavailable` | `functionCallId`, `requested`, `error` |

## Residual risks

| Risk | Status |
|---|---|
| A process that calls `setsid` outlives the call on the host and under Seatbelt | Documented; bubblewrap ends it with the PID namespace |
| A protected name that does not exist below the root (a new nested `AGENTS.md`, `.envrc` or `.git`) can be created by a sandboxed command | Root-level names gate `auto_allow_sandboxed`; nested files are listed, never inlined, by the instruction loader |
| On a case-sensitive file system a differently cased protected name (`agents.md`) is a different file | File tools match protected names case-folded; the sandbox protects names that exist |
| Network access is all or nothing | `network: false` is required for unapproved sandboxed commands |
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
