# adk-devtools

Developer tools for ADK-Rust coding agents — the inner-loop tools an agent needs
to work on a codebase, scoped to a sandboxed workspace.

| Tool | Purpose |
|------|---------|
| `read_file` | Read a text file with line numbers (optional offset/limit). |
| `write_file` | Create or overwrite a file (creates parent dirs). |
| `edit_file` | Exact-string replacement; requires a prior `read_file`. |
| `glob` | List files matching a pattern (e.g. `src/**/*.rs`). |
| `grep` | Regex content search across the tree. |
| `bash` | Run a shell command in the workspace root, with a timeout. Streams stdout/stderr line-by-line via `ToolContext::emit_progress`. |

All operations are rooted at a [`Workspace`] and rejected if they escape it.
Mutations require a writable workspace; `bash` requires bash to be enabled.

The `bash` tool emits its output incrementally as it runs, so UIs can display a
live terminal (see the `streaming_bash` example). The framework forwards each
chunk as a partial event on the agent's `EventStream`; consumers detect them
with `event.tool_progress_stream()`. The output is also returned as the tool's
final result for the model, capped per stream at `Workspace::max_output_bytes`
(default 1 MiB) as it is read; excess output is drained and discarded, and the
result sets `truncated: true`.

## Usage

```rust,ignore
use adk_devtools::{DevToolset, Workspace};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let workspace = Workspace::new("./my-repo")            // read-write, bash off
    .allow_bash(true);                                 // opt in to the shell
// let workspace = Workspace::read_only("./my-repo");  // explore / plan mode

let agent = LlmAgentBuilder::new("coding-agent")
    .model(model)
    .toolset(Arc::new(DevToolset::new(workspace)))
    .build()?;
```

The toolset only exposes tools the workspace permits: a read-only workspace hides
`write_file`/`edit_file`/`bash`.

## Sandboxing

`Workspace` enforces **path containment**, **read-only** mode, and a **bash
timeout**. Containment is checked against resolved paths: a symlink pointing
outside the root is refused, a dangling or looping symlink is refused wherever it
points (a write would create its target), and `glob` refuses `..` and absolute
patterns and never lists through a symlinked directory that leads outside. A
timed-out or cancelled `bash` call kills the command's whole process group.
Phase 1 runs `bash` host-local (`sh -c`, cwd pinned to the root); it is
not strongly isolated. For strong isolation, run `bash` behind a containerized
`CodeExecutor`. The policy vocabulary aligns with `adk-code`'s `SandboxPolicy` and
will integrate with it directly in a later phase.

See `docs/design/coding-agent.md` for the overall coding-agent design.
