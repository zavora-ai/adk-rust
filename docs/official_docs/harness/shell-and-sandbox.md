# Harness Shell and Sandbox

The `HarnessAgent` (`adk-harness`) runs model-written shell commands through one `bash`
tool. By default the command runs on the host behind a per-command approval; the OS sandbox
is opt-in, and a strict deployment can refuse to start without it. This page covers the
sandboxed shell: what it can reach, how a failed command is retried outside it, and what
the harness records.

## Isolation

| `ShellIsolation` | Where `bash` runs | Reported `IsolationStatus` |
|---|---|---|
| `Host` (default) | `sh -c` on the host, with the user's permissions and network | `Host` |
| `OsSandbox(OsSandboxConfig)` | `sh -c` under Seatbelt (macOS) or bubblewrap (Linux) | `OsSandbox { enforcer }` |
| `OsSandbox(..)` with no working enforcer | the host, as `Host` | `Degraded { requested, reason }` |

The enforcer is probed when the harness is built and again on every call:

| Situation | Result |
|---|---|
| Enforcer unavailable at build, `fail_if_unavailable` set | `build()` fails with `harness.sandbox_unavailable` |
| Enforcer unavailable at build, no `fail_if_unavailable` | A warning; the shell is `Degraded`: it declares and behaves as the host shell, every command needs approval in Ask, and `auto_allow_sandboxed` has no effect |
| Enforcer available at build, unavailable at a call | The call is refused with `harness.sandbox_unavailable` and a `harness.sandbox.unavailable` record; it never falls back to the host |

The Windows AppContainer enforcer is not implemented, so on Windows the shell is `Host`, or
`Degraded` when a sandbox is requested.

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

## Modes and approvals

| Mode | Sandboxed `bash` | Unsandboxed retry |
|---|---|---|
| ReadOnly | Deny | Deny |
| Plan | Allow, read-only workspace, under an active enforcer only | Deny |
| Ask (default) | Approval; Allow with `auto_allow_sandboxed` | Approval, non-waivable |
| Auto | Allow when sandboxed commands may run unapproved (below); Approval otherwise | Approval, non-waivable |

The `bash` declaration's annotations are `{enforcer, profileDigest, network}`, where
`profileDigest` covers the enforcer, network, process spawning, toolchain reads and extra
paths. A standing rule or a held call is bound to the declaration digest, so a rule granted
under one sandbox configuration never applies under another, nor on the host.

### `auto_allow_sandboxed`

Sandboxed commands run without approval in Ask (`auto_allow_sandboxed`), and in Auto, only
when every condition holds:

| Condition | Checked |
|---|---|
| An OS sandbox is active (not `Host`, not `Degraded`) | At build; refused per call if it goes away |
| `network` is `false` | At build |
| The enforcer keeps a read-only path inside a read-write path read-only: a conformance script tries to write, create, remove, rename and link beneath one | At build |
| Every root-level protected path exists and is not a symbolic link | At build and on every call |

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

| Guarantee | Host | Seatbelt | bubblewrap |
|---|---|---|---|
| Standard input is `/dev/null` | yes | yes | yes |
| New session, no controlling terminal | `setsid` | `setsid` | `setsid` and `--new-session` |
| Background processes die when the command exits | process-group kill | process-group kill | the PID namespace ends with `bwrap` |
| A freeze kills the running command | within 250 ms of the poll | within 250 ms of the poll | within 250 ms of the poll |

A process that starts its own session (`setsid`) leaves the process group and survives the
group kill on the host and under Seatbelt; bubblewrap's PID namespace still ends it.

## Audit records

| Action | Fields (`ext`) |
|---|---|
| `harness.bash.begin` (write-ahead, `args_digest`, `payload_ref` = digest of the command) | `functionCallId`, `enforcer`, `profileDigest`, `network`, `workspaceAccess`, `envKeys`, `retryOf`, `goalCheck` |
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
