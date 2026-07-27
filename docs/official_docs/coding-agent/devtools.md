# Dev Tools (`adk-devtools`)

`adk-devtools` is the inner-loop toolset a coding agent needs — read, edit,
search, and run — with every operation **scoped to a workspace directory**. It's
a standalone, publishable crate depending only on `adk-core`, so it composes with
any `LlmAgent` (the [`CodingAgent`](harness.md) harness wires it for you).

## The tools

`DevToolset` is a `Toolset` bundling six tools:

| Tool | Params | Behavior |
|------|--------|----------|
| `read_file` | `path`, `offset?`, `limit?` | Return file contents, line-numbered |
| `write_file` | `path`, `content` | Create/overwrite a file (creates parent dirs) |
| `edit_file` | `path`, `old_string`, `new_string`, `replace_all?` | Exact-string replacement |
| `glob` | `pattern`, `path?` | List files matching a glob (e.g. `src/**/*.rs`) |
| `grep` | `pattern`, `path?`, `glob?`, `case_insensitive?` | Regex content search |
| `bash` | `command`, `timeout_secs?` | Run a shell command in the workspace root |

Two safety behaviors worth knowing:

- **`edit_file` requires a prior `read_file`** of that file in the session, and
  by default the target string must occur **exactly once** (`replace_all` to
  override). This guards against blind overwrites.
- **`grep`** skips common build/VCS dirs (`target`, `.git`, `node_modules`, …)
  and binary/oversized files.

The `bash` tool **streams** its stdout/stderr line-by-line via
`ToolContext::emit_progress` as the command runs, so UIs can show a live
terminal. Each chunk arrives as a partial event on the agent's `EventStream`
(detect with `event.tool_progress_stream()`); the complete output is still
returned as the tool's final result. See the
[`streaming_bash` example](../events/events.md#streaming-tool-progress) and
[Streaming Progress from a Tool](../tools/function-tools.md#streaming-progress-from-a-tool).

## The `Workspace`

A `Workspace` roots every operation at a directory and enforces a small policy:

```rust
use adk_devtools::Workspace;
use std::time::Duration;

let ws = Workspace::new("./my-repo");              // read-write, bash enabled
let ws = Workspace::read_only("./my-repo");        // explore/plan: no writes, no bash
let ws = Workspace::new("./my-repo")
    .allow_bash(false)                              // file edits, but no shell
    .bash_timeout(Duration::from_secs(60))
    .max_output_bytes(512 * 1024);
```

- **Path containment** — any path that resolves outside the root is rejected, so
  the agent can't read or write `../../etc/...`. Containment is enforced against the
  **resolved** path, not just the literal one: a symlink pointing outside the root is
  rejected even though it sits lexically inside. That covers a symlinked final
  component and a symlinked parent directory, so creation through a redirected
  directory is refused too. A symlink whose target stays inside the workspace keeps
  working, since repositories legitimately contain internal links.

  The check is not a lock. A symlink planted between the check and the subsequent
  open would still be followed; closing that window needs descriptor-relative
  traversal with platform no-follow semantics. Treat the file tools as containment
  against an agent that wanders, not as isolation against an adversary that can
  write into the workspace concurrently.
- **Read-only mode** — `Workspace::read_only(..)` hides the mutating tools
  entirely (the model only ever sees `read_file`/`glob`/`grep`).
- **`bash` environment is cleared** — the command receives only `PATH`, `HOME`, `LANG`,
  `LC_ALL`, `TMPDIR`, `TERM`, `USER`, and `SHELL`, so provider API keys held by the agent
  process are not readable with `env`. `Workspace::inherit_env(true)` restores the old
  pass-everything behaviour, and `env_allowlist` replaces the set.
- **`bash` timeout + output caps** — long or chatty commands are bounded. A timed-out
  command is killed as a **process group**, so anything it started is killed too;
  previously only the direct child was signalled and descendants survived.

## Using it directly

Attach the toolset to any agent:

```rust
use adk_devtools::{DevToolset, Workspace};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("coder")
    .model(model)
    .toolset(Arc::new(DevToolset::new(Workspace::new("./my-repo"))))
    .build()?;
```

`DevToolset` only exposes the tools the workspace permits, so a read-only
workspace yields a read-only agent automatically.

## Sandboxing model

Phase 1 runs `bash` **host-local** (`sh -c`, working directory pinned to the root) with a
timeout and a cleared environment. What that does and does not give you:

| Enforced | Not enforced |
|----------|--------------|
| File tools cannot resolve outside the root, including through symlinks | `bash` can still use absolute paths — the working directory is not an OS boundary |
| The command cannot read the agent's environment variables | The command can reach the network |
| A timeout kills the command and its descendants | Nothing limits memory or CPU |

So it is path-contained, environment-isolated, and bounded, but **not** OS-isolated. The policy vocabulary aligns with `adk-code`'s `SandboxPolicy`; for
strong isolation, run `bash` behind a containerized executor (see the
[design doc](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/coding-agent.md#9-security--sandboxing)).
Combine with [`adk-guardrail`](../security/guardrails.md) (command allowlists,
secret redaction) and [`adk-auth`](../security/access-control.md) for tokened
tools (e.g. GitHub).

Next: [The harness →](harness.md)
