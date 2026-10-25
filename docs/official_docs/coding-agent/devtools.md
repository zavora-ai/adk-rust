# Dev Tools (`adk-devtools`)

> **Note:** the [harness](../harness/index.md) builds on these tools and supersedes
> `adk_agent::coding::CodingAgent` in 3.1; see
> [Migrating from `CodingAgent`](../migration/coding-agent-to-harness.md).

`adk-devtools` is the inner-loop toolset a coding agent needs — read, edit,
search, and run — with every operation **scoped to a workspace directory**. It's
a standalone, publishable crate depending only on `adk-core`, so it composes with
any `LlmAgent` (the [`CodingAgent`](harness.md) harness wires it for you).

## The tools

`DevToolset` is a `Toolset` bundling six tools:

| Tool | Params | Behavior |
|------|--------|----------|
| `read_file` | `path`, `offset?`, `limit?` | Return file contents, line-numbered; a PNG, JPEG, GIF, WebP or PDF as inline data (see [Images and PDFs](#images-and-pdfs)) |
| `write_file` | `path`, `content` | Create/overwrite a file (creates parent dirs) |
| `edit_file` | `path`, `old_string`, `new_string`, `replace_all?` | Exact-string replacement |
| `glob` | `pattern`, `path?` | List files matching a glob (e.g. `src/**/*.rs`) |
| `grep` | `pattern`, `path?`, `glob?`, `case_insensitive?` | Regex content search |
| `bash` | `command`, `timeout_secs?` | Run a shell command in the workspace root |

Three safety behaviors worth knowing:

- **`edit_file` requires a prior `read_file`** of that file by the same session
  and agent, and by default the target string must occur **exactly once**
  (`replace_all` to override). This guards against blind overwrites. The record of
  reads is kept per (app, user, session, agent), taken from the tool call's
  context, so clones of one `Workspace` shared by several sessions or team members
  keep their reads apart; at most 1,024 records are kept, least recently used
  first out. Calls whose context names no session share one record.
- **`grep`** skips common build/VCS dirs (`target`, `.git`, `node_modules`, …)
  and binary/oversized files.
- **Secret files are excluded.** `grep` and `glob` skip files matching the
  workspace's exclusion patterns and `read_file` refuses them (see
  [Secret files](#secret-files)).

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

let ws = Workspace::new("./my-repo");              // read-write, no shell
let ws = Workspace::read_only("./my-repo");        // explore/plan: no writes, no bash
let ws = Workspace::new("./my-repo")
    .allow_bash(true)                               // opt in to the shell
    .bash_timeout(Duration::from_secs(60))          // a call may ask for less, not more
    .max_output_bytes(512 * 1024)
    .exclude_globs([".env".to_string(), "secrets/**".to_string()]) // replace the secret patterns
    .bash_kill_group_on_exit(true);                 // default: no background survivors
```

- **Path containment** — any path that resolves outside the root is rejected, so
  the agent can't read or write `../../etc/...`. Containment is enforced against the
  **resolved** path, not just the literal one: a symlink pointing outside the root is
  rejected even though it sits lexically inside. That covers a symlinked final
  component and a symlinked parent directory, so creation through a redirected
  directory is refused too. A symlink whose target stays inside the workspace keeps
  working, since repositories legitimately contain internal links.

  A **dangling or looping symlink** anywhere on the path is refused wherever it
  points, because a write would follow it and create its target — a cloned
  repository containing `link -> ~/.zshenv` must not let `write_file("link", ..)`
  create `~/.zshenv`. Write to the link's target path directly instead.

  `glob` refuses patterns containing `..` or an absolute path, and drops every
  match whose resolved path lies outside the root, so a symlinked directory
  pointing elsewhere is not listed.

  The check resolves each path once, to its canonical target, and on Unix
  `read_file`, `edit_file` and `write_file` open that target without following a
  symlink on any component: Linux uses `openat2` with `RESOLVE_BENEATH |
  RESOLVE_NO_SYMLINKS`, and other systems, or a Linux kernel without `openat2`,
  open one component at a time with `O_NOFOLLOW` relative to the parent's
  descriptor. `write_file` opens the parent directory by descriptor, creating
  missing directories the same way, and the file relative to it. A file or
  directory swapped for a symlink after the check — by a concurrent `bash`
  command, say — fails the call with the escape error instead of redirecting it,
  whether the link leads outside the root or to another file inside it.

  | Swap after the check | Result |
  |----------------------|--------|
  | A file replaced by a symlink to a host file | `read_file` fails; the host file is not read |
  | A file replaced by a symlink to another workspace file | `write_file` and `edit_file` fail; the other file is unchanged |
  | A directory on the path replaced by a symlink | Every file tool fails; nothing is read or created through it |

  A symlink that already exists when the check runs and stays inside the root is
  followed. The guarantee covers the window from the tool's check to its open: a
  link changed between a harness's approval decision and the tool's check still
  redirects the call, within the root.

  A file with more than one hard link is not read: `read_file` and `edit_file`
  refuse it, since another link to the same file can lie outside the workspace and
  the workspace cannot tell. `write_file` replaces it, keeping its permissions,
  rather than truncating it, so a link from outside the workspace keeps its
  contents.

  On Windows the same three tools verify each open on the handle it returns. The
  target is opened with `FILE_FLAG_OPEN_REPARSE_POINT`, without truncation, and the
  call fails with the escape error when the handle is a symlink or a junction — a
  name-surrogate reparse point — or when `GetFinalPathNameByHandleW` names anything
  but the checked target beneath the root's own final path. `write_file` opens or
  creates each directory on the path the same way and holds it open until the write
  ends, so none can be renamed or replaced by a link meanwhile, and truncates the file
  only after every check. The hard-link refusal applies as on Unix.

  | Windows entry on the path | Result |
  |---------------------------|--------|
  | A symlink or junction swapped in after the check | The call fails; nothing is read or created through it |
  | An in-place reparse point: a WOF-compressed file, a cloud placeholder (OneDrive Files On-Demand) | Not refused; it is reopened through its handle, so its filter serves the content |
  | A volume mounted in a folder without a drive letter | The call fails: the final path cannot be named |
- **Read-only mode** — `Workspace::read_only(..)` hides the mutating tools
  entirely (the model only ever sees `read_file`/`glob`/`grep`).
- **`bash` environment is cleared** — the command receives only `PATH`, `HOME`, `LANG`,
  `LC_ALL`, `TMPDIR`, `TERM`, `USER`, and `SHELL`, so provider API keys held by the agent
  process are not readable with `env`. `Workspace::inherit_env(true)` passes the full
  environment, and `env_allowlist` replaces the set.
- **`bash` timeout + output caps** — long or chatty commands are bounded. A timed-out
  command is killed as a **process group**, so anything it started is killed too. Dropping
  the tool call's future (for example when the run is cancelled) kills the group the
  same way. Each stream keeps at most `max_output_bytes` (default 1 MiB) as it is
  read; output past the cap is drained and discarded, so the command still runs to
  completion and reports its exit code, and the result sets `truncated: true`. The
  cut falls on a UTF-8 character boundary.
- **`bash` leaves nothing behind** — the command's standard input is `/dev/null`, so
  `read` sees end of file instead of the agent's terminal, and it starts in a new
  session (`setsid`) with no controlling terminal, leading its own process group.
  When the shell exits, the rest of the group is killed: `sleep 300 &` or a dev
  server started in the background does not outlive the call, and the call does not
  wait for it to close its output. A process that starts its own session or group
  escapes the kill. `Workspace::bash_kill_group_on_exit(false)` lets background
  processes keep running; a timeout or a cancelled call still kills the group.
  On Windows the command starts suspended and runs only once it is in a **Job
  Object**, so everything it starts belongs to the job and none of it can leave; the
  job's processes are killed where Unix kills the group, and when the agent process
  exits during the call.

### Secret files

A new workspace excludes `DEFAULT_EXCLUDE_GLOBS`: `.env`, `.env.*`, `*.pem`,
`*.key`, `id_rsa*`, `id_ed25519*`, `*.p12` and `*.pfx`.

| Tool | Behaviour on an excluded file |
|------|-------------------------------|
| `grep` | Skipped, including when named as the search path; an excluded directory is not descended |
| `glob` | Not listed |
| `read_file` | Refused with `DevToolError::Excluded` — an `AdkError` with category `Forbidden` and code `devtools.path_excluded` |

A pattern matches, ignoring case, the path relative to the root or any one of its
components, so `.env` also covers `config/.env` and `.ENV`. `read_file` checks the
path a symlink resolves to as well, so a link named `settings.txt` pointing at
`.env` is refused. `Workspace::exclude_globs` replaces the list and
`exclude_globs([])` turns exclusion off; `Workspace::is_excluded` answers the same
question for a path, so a harness can require approval before reading one.

### Errors the model sees

Each `DevToolError` maps to an `AdkError` with a code. The ones a model can act on are
rejections, so `LlmAgent` passes their message to the model (spec 3.1 §7.33 fix 6):

| Error | Category | Code | The model sees |
|---|---|---|---|
| `NotRead` | `Conflict` | `devtools.not_read` | `file 'src/lib.rs' must be read with read_file before it can be edited` |
| `NoMatch`, `Ambiguous` | `InvalidInput` | `devtools.no_match`, `devtools.ambiguous` | The string was not found, or how many times it occurs |
| `PathEscape`, `ReadOnly`, `BashDisabled`, `Excluded` | `Forbidden` | `devtools.path_escape`, `devtools.read_only`, `devtools.bash_disabled`, `devtools.path_excluded` | Why the call is refused |
| `Io` of a missing file | `NotFound` | `devtools.not_found` | The I/O message |
| `Other` (bad arguments, a bad pattern) | `InvalidInput` | `devtools.invalid_argument` | The message |
| `Timeout` | `Timeout` | `devtools.timeout` | The category and code only |
| Any other `Io` | `Internal` | `devtools.io` | The category and code only |

## Using it directly

Attach the toolset to any agent:

```rust
use adk_devtools::{DevToolset, Workspace};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("coder")
    .model(model)
    .toolset(Arc::new(DevToolset::new(Workspace::new("./my-repo").allow_bash(true))))
    .build()?;
```

`DevToolset` only exposes the tools the workspace permits, so a read-only
workspace yields a read-only agent automatically.

## Sandboxing model

`bash` runs **host-local** (`sh -c`, working directory pinned to the root) with a
timeout and a cleared environment, and only when the workspace allows it
(`Workspace::allow_bash(true)`; `Workspace::new` leaves it off). What that does and does not give you:

| Enforced | Not enforced |
|----------|--------------|
| File tools cannot resolve outside the root, including through symlinks, and on Windows junctions, swapped in after the check; they do not read hard-linked files | `bash` can still use absolute paths — the working directory is not an OS boundary |
| The command cannot read the agent's environment variables | The command can reach the network |
| A timeout, a cancelled call or the shell's exit kills the command's process group, or on Windows its Job Object | On Unix, a descendant that calls `setsid` or `setpgid` leaves the group and survives |
| The command has no controlling terminal and reads `/dev/null` | Nothing limits the command's own memory or CPU |
| Captured output is capped per stream as it is read | `bash` can read excluded secret files; exclusion applies to the file tools |

### Command launchers

`BashTool::with_launcher` replaces how the process is built while the tool keeps
the timeout ceiling, output cap, streaming, standard input, session and group
kill. A `CommandLauncher` returns the `tokio::process::Command` for a script and
names its isolation; `HostLauncher` (`sh -c` in the root, the workspace
environment) is the default.

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

The launcher must not set a process group or session on the command: the tool
starts it with `setsid`, which fails for a process that already leads a group. On
Windows the tool replaces the command's creation flags with `CREATE_SUSPENDED`, so
the process runs only once it is in the call's Job Object.

On Windows a launcher whose confinement is set when the process is created — the
restricted-token sandbox, whose token cannot be attached to a `Command` — starts the
process itself by overriding `CommandLauncher::spawn_contained` (spec 3.2 §10 W5). It
returns an `adk_sandbox::process_tree::ContainedChild`: the process, already in its Job
Object, with its standard streams on pipes. `BashTool` closes its standard input at once
and keeps the timeout ceiling, output cap, streaming and the job's kill around it, as for a
command it spawns; the call runs on Tokio's blocking pool. The default returns `None`,
and the tool spawns `command`'s result.

So it is path-contained, environment-isolated, and bounded, but **not** OS-isolated. The policy vocabulary aligns with `adk-code`'s `SandboxPolicy`; for
strong isolation, run `bash` behind a containerized executor (see the
[design doc](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/coding-agent.md#9-security--sandboxing)).
Combine with [`adk-guardrail`](../security/guardrails.md) (command allowlists,
secret redaction) and [`adk-auth`](../security/access-control.md) for tokened
tools (e.g. GitHub).

Next: [The harness →](harness.md)

## Images and PDFs

`read_file` returns a file whose bytes start like a PNG, JPEG, GIF, WebP or PDF as inline
data (spec 3.1 §10 D6): the result is `{ "response": { path, mime_type, bytes, content,
inline }, "inline_data": [{ mime_type, data }] }`, which the agent loop turns into a function
response with an inline part. Providers with vision receive the picture or document;
`content` is a text stub naming the type and size, so a provider that drops binary parts
still learns what the file is.

| Rule | Mechanism |
|---|---|
| The type comes from the bytes | `sniff_inline_mime` reads the magic bytes; a text file named `diagram.png` is read as text and `notes.txt` holding a PNG is returned as an image |
| Same confinement as text | The bytes are read through the no-follow open and hard-link refusal of the section above, and excluded files stay refused |
| Capped | `ReadFileTool::max_inline_bytes` (default `DEFAULT_MAX_INLINE_BYTES`, 4 MiB; at most `adk_core::MAX_INLINE_DATA_SIZE`): a larger file gets the stub without bytes |
| Text-only models | `ReadFileTool::text_only` returns the stub and never the bytes |
| Nothing else | Other binary content is refused, as before; the bytes are never interpreted by the tool |
