# adk-devtools

Developer tools for ADK-Rust coding agents — the inner-loop tools an agent needs
to work on a codebase, scoped to a sandboxed workspace.

[![Crates.io](https://img.shields.io/crates/v/adk-devtools.svg)](https://crates.io/crates/adk-devtools)
[![Documentation](https://docs.rs/adk-devtools/badge.svg)](https://docs.rs/adk-devtools)
[![License](https://img.shields.io/crates/l/adk-devtools.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

| Tool | Purpose |
|------|---------|
| `read_file` | Read a text file with line numbers (optional offset/limit). Refuses excluded secret files. |
| `write_file` | Create or overwrite a file (creates parent dirs). |
| `edit_file` | Exact-string replacement; requires a prior `read_file` by the same session and agent. |
| `glob` | List files matching a pattern (e.g. `src/**/*.rs`). Skips excluded secret files. |
| `grep` | Regex content search across the tree. Skips excluded secret files. |
| `bash` | Run a shell command in the workspace root, with a timeout. Opt-in. Streams stdout/stderr line-by-line via `ToolContext::emit_progress`. |

All operations are rooted at a `Workspace` and rejected if they escape it.
Mutations require a writable workspace; `bash` requires `Workspace::allow_bash(true)`.

The `bash` tool emits its output incrementally as it runs, so UIs can display a
live terminal (see the
[`streaming_bash`](https://github.com/zavora-ai/adk-rust/tree/main/examples/streaming_bash)
example). The framework forwards each chunk as a partial event on the agent's `EventStream`;
consumers detect them with `event.tool_progress_stream()`. The output is also returned as the
tool's final result for the model, capped per stream at `Workspace::max_output_bytes`
(default 1 MiB) as it is read; excess output is drained and discarded, and the
result sets `truncated: true`.

## Installation

```toml
[dependencies]
adk-devtools = "3.0"
```

The crate has no feature flags.

## Usage

```rust
use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::Llm;
use adk_devtools::{DevToolset, Workspace};
use std::sync::Arc;
use std::time::Duration;

fn coding_agent(model: Arc<dyn Llm>) -> adk_core::Result<LlmAgent> {
    let workspace = Workspace::new("./my-repo") // read-write, bash off
        .allow_bash(true) // opt in to the shell
        .bash_timeout(Duration::from_secs(60));
    // let workspace = Workspace::read_only("./my-repo"); // explore / plan mode

    LlmAgentBuilder::new("coding-agent")
        .model(model)
        .toolset(Arc::new(DevToolset::new(workspace)))
        .build()
}
```

The toolset only exposes tools the workspace permits: a read-only workspace hides
`write_file`/`edit_file`/`bash`, and `bash` is hidden until `allow_bash(true)`.

## Sandboxing

| Control | Default | Builder |
|---------|---------|---------|
| Path containment | Always on | — |
| Writes | Allowed (`Workspace::new`); denied (`Workspace::read_only`) | `writable` |
| `bash` | Off | `allow_bash` |
| `bash` timeout | 120 s; a call's `timeout_secs` can shorten it but not extend it | `bash_timeout` |
| `bash` environment | Cleared, then `DEFAULT_ENV_ALLOWLIST` (`PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`, `TERM`, `USER`, `SHELL`) | `env_allowlist`, `inherit_env` |
| `bash` background processes | Killed when the shell exits | `bash_kill_group_on_exit` |
| Output per stream | 1 MiB | `max_output_bytes` |
| Secret files | `DEFAULT_EXCLUDE_GLOBS` (`.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `*.p12`, `*.pfx`) skipped by `grep`/`glob`, refused by `read_file`; `exclude_globs([])` turns exclusion off | `exclude_globs` |
| Read-before-edit record | Per session and agent; clones of a workspace keep sessions apart | — |

Containment is checked against resolved paths: a symlink pointing outside the root is refused,
a dangling or looping symlink is refused wherever it points (a write would create its target),
and `glob` refuses `..` and absolute patterns and never lists through a symlinked directory that
leads outside. On Unix, `read_file`, `edit_file` and `write_file` open the canonical target the
check found without following a symlink on any component (`openat2` with `RESOLVE_BENEATH |
RESOLVE_NO_SYMLINKS` on Linux, a component-by-component `O_NOFOLLOW` walk elsewhere), so a file
or directory swapped for a symlink after the check fails the call. `read_file` and `edit_file`
refuse a file with more than one hard link; writing one replaces it, keeping its permissions,
instead of truncating it, so a link outside the workspace keeps its contents. Windows keeps the
path checks without the descriptor-relative opens or the link-count refusal. A timed-out or
cancelled `bash` call kills the command's whole process group.

Exclusion patterns match, ignoring case, the path relative to the root or any one of its
components, so `.env` also covers `config/.env`; `read_file` checks the path a symlink resolves
to as well. A refused read is an error with category `Forbidden` and code
`devtools.path_excluded`, so a harness can ask for approval and read the file through a workspace
that does not exclude it.

`bash` runs host-local (`sh -c`, cwd pinned to the root); it is not isolated, and a command can
reach absolute paths and the network. The environment is cleared so a command cannot read
provider API keys from the agent process with `env`; adding a credential to the allowlist, or
`inherit_env(true)`, exposes it again. Standard input is `/dev/null`, and the command starts in a
new session with no controlling terminal, leading its own process group. When the shell exits,
the rest of the group is killed, so `sleep 300 &` or a server started in the background does not
outlive the call; a process that starts its own session or group escapes this. Pass
`bash_kill_group_on_exit(false)` to let background processes keep running.

### Command launchers

`BashTool::with_launcher` replaces how the process is built. A `CommandLauncher` returns the
`tokio::process::Command` for a script — program, arguments, working directory and
environment — and names its isolation; the tool keeps the timeout ceiling, output cap,
streaming, standard input, session and group kill. `HostLauncher` (`sh -c`, the default)
reproduces the built-in behaviour. A sandboxing launcher can wrap the script with an
`adk-sandbox` enforcer.

```rust
use std::sync::Arc;

use adk_core::ToolContext;
use adk_devtools::{BashTool, CommandLauncher, DevToolError, Workspace};

/// Runs every script under `nice`, otherwise as the host launcher does.
struct NiceLauncher;

impl CommandLauncher for NiceLauncher {
    fn command(
        &self,
        ws: &Workspace,
        script: &str,
        _ctx: &dyn ToolContext,
    ) -> Result<tokio::process::Command, DevToolError> {
        let mut cmd = tokio::process::Command::new("nice");
        cmd.args(["sh", "-c", script]).current_dir(ws.root());
        if !ws.inherits_env() {
            cmd.env_clear().envs(ws.bash_env());
        }
        Ok(cmd)
    }

    fn isolation(&self) -> &str {
        "host-nice"
    }
}

fn bash_tool() -> BashTool {
    let workspace = Workspace::new("./my-repo").allow_bash(true);
    BashTool::new(workspace).with_launcher(Arc::new(NiceLauncher))
}
```

For strong isolation, run commands through a sandboxing launcher, a containerized `adk-code`
executor or an `adk-sandbox` backend. The policy vocabulary aligns with `adk-code`'s
`SandboxPolicy`.

See [`docs/design/coding-agent.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/coding-agent.md)
for the overall coding-agent design.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
