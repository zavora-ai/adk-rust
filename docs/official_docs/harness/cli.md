# The harness in your terminal

`adk-rust` with no subcommand is the harness: the governed coding agent of spec 3.1
(`adk-harness`) in the current directory. It reads and edits files confined to the workspace,
runs commands with `bash` behind per-call approval, keeps a todo list, follows the
workspace's `AGENTS.md` once you trust it, and remembers the decisions you ask it to.

```bash
cargo install adk-cli          # default features include `harness` and `persist`
cd my-project
adk-rust                       # the harness REPL, ask mode
```

> **Note:** the harness refuses to start in your home directory or `/` unless `--dir` names
> them explicitly, since the agent could read and change everything under them.

## Commands

| Invocation | Behaviour |
|---|---|
| `adk-rust` | Harness REPL in `--dir` (default `.`), ask mode, approvals in the terminal |
| `adk-rust chat` | The plain chat agent, without workspace tools |
| `adk-rust code "task"` | One harness turn; exit code `3` means calls are held for a decision (the session id is printed) |
| `adk-rust goal "goal" --until CMD` | Iterates harness turns until `CMD` exits 0; `--resume` continues the goal and its conversation |
| `adk-rust ultracode "task"` | Implements in your mode, reviews with three read-only reviewers in parallel, revises until they approve |
| `adk-rust serve` | The harness in plan mode behind the HTTP server with the approval inbox; see [Serving the harness](serving.md) |
| `adk-rust sessions list\|show\|rm` | Your harness sessions |
| `adk-rust rules list\|add\|rm` | Your standing rules |
| `adk-rust approvals list\|approve\|deny` | Calls waiting for a decision, here or on a served harness with `--server URL --token T` |

The model is chosen as for every command: `--provider`, `--model` and `--api-key`, the
provider's environment variable, secure storage, the saved config, or first-run setup on a
terminal.

## Shared flags

The REPL, `code`, `goal`, `ultracode` and `serve` take the same harness flags.

| Flag | Effect |
|---|---|
| `--dir DIR` | The workspace; file tools never leave it |
| `--mode read-only\|plan\|ask\|auto` | The session's mode; `serve` defaults to `plan`, everything else to `ask` |
| `--yes` | Approves every call that reaches the prompt; never overrides a policy denial and never approves an unsandboxed retry without `--allow-unsandboxed` |
| `--session ID` / `--continue` / `--ephemeral` | Continue a session (creating it when new), continue the latest session in this workspace, or keep nothing on disk |
| `--sandbox` | Runs `bash` in the OS sandbox (Seatbelt on macOS, bubblewrap on Linux) |
| `--fail-if-unavailable` | With `--sandbox`: refuse to start when the sandbox is unavailable instead of running `bash` on the host |
| `--allow-unsandboxed` | Lets `--yes` approve an unsandboxed retry of a command the sandbox refused |
| `--trust` | Trusts the workspace's instruction files without asking |
| `--pack browser\|computer-use\|payments` | Adds a pack |
| `--member NAME=explore\|general` | Adds a team member |

> **Note:** packs, ACP and A2A members and `/memory` report themselves unavailable until
> their parts of spec 3.1 land (§7.10, §7.15, §7.16).

### Configuration files

Settings are read in this order; the first that sets a value wins:

1. Flags.
2. `.adk/harness.toml` in the workspace.
3. `<config dir>/adk-rust/harness.toml` (on macOS `~/Library/Application Support/adk-rust/`,
   on Linux `~/.config/adk-rust/`).
4. Defaults.

Both files take the same keys:

```toml
mode = "plan"                # read-only, plan, ask or auto
sandbox = true
fail_if_unavailable = true
yes = false
allow_unsandboxed = false
trust = false
packs = []
members = ["scout=explore"]
```

A workspace file can only tighten (spec 3.1 §9.2 I3): it may choose a stricter mode than
your config, turn the sandbox on, or require it. `yes`, `trust`, `allow_unsandboxed`,
`packs`, `members` and a looser mode in `.adk/harness.toml` are ignored, and the banner says
which. A cloned repository therefore cannot approve its own commands.

## Approvals at the terminal

In ask mode every edit and every host command waits for you:

```text
[approval needed] bash on host (full user permissions, network)
  $ cargo test
  shell · mode ask · host (full user permissions, network) · origin static (sha256:8ce5…)
  agent urn:adk:agent:adk-rust/adk-rust for ana
  [y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details > a
  remember which rule?
    [1] bash: cargo test … (every session) — runs project scripts, so it allows whatever they do
    [2] bash: cargo … (every session) — runs project scripts, so it allows whatever they do
  rule [1-2], or Enter to go back > 1
```

| Answer | Effect |
|---|---|
| `y` | Runs the call once, with your edited arguments when `e` changed them |
| `a` | Lists the standing rules this call can become; the chosen rule is stored and the call runs |
| `n` | Denies the call; a reason you type is reported to the model, and the turn ends once the denial is recorded |
| `e` | Replaces the arguments with JSON from `$VISUAL`/`$EDITOR`, or typed on one line; the policy checks the edited call again |
| `?` | Shows the full call; a call cut for display can be approved only after `?` |
| Ctrl+C | Denies this call and the rest of the turn, and cancels the run |

`[a]` appears only when a rule can cover the call. `bash` rules match plain command prefixes
(`cargo test …`), never a command with `;`, `&&`, pipes, redirections or substitutions, and
never one starting with `sh`, `bash`, `env`, `sudo` and similar. A rule belongs to you,
applies in every session of `adk-rust`, and is bound to the tool's declaration: a rule
granted for host `bash` does not apply to sandboxed `bash`, and a changed tool no longer
matches. `exit_plan_mode`, protected-path edits and unsandboxed retries are decided every
time.

Without a terminal the prompt denies each call and says how to approve it. One-shot commands
(`code`, `goal`, `ultracode`) hold the calls instead:

```bash
adk-rust code "make the failing test pass" < /dev/null
# 1 call(s) are held for a decision in session 6754153c-….
adk-rust approvals list
adk-rust approvals approve 3f9a1c0d2b7e
adk-rust code --session 6754153c-… "continue"     # the approved call runs first, once
```

## Modes

| Mode | Reads, todos | Edits | Host `bash` |
|---|---|---|---|
| `read-only` | allowed | refused | refused |
| `plan` | allowed | refused | refused; the model presents a plan with `exit_plan_mode` |
| `ask` | allowed | ask | ask |
| `auto` | allowed | allowed (protected paths still ask) | ask |

`/mode` in the REPL changes the session's mode. A looser mode first cancels every call held
in the session, so nothing you have not seen runs under the looser mode. Standing rules
apply where the mode would ask.

## The REPL

The banner shows the workspace, mode, shell isolation, team members and session id. The
first time a workspace has instruction files (`AGENTS.md`, or a fallback name without one), the
REPL lists them with their sizes and hashes and asks whether to trust them; untrusted files
are listed to the model but not loaded.

| Command | Effect |
|---|---|
| `/mode [read-only\|plan\|ask\|auto]` | Shows or sets the session's mode |
| `/sandbox [on\|off\|status]` | Shows the shell isolation, or rebuilds the harness with the sandbox on or off |
| `/todos` | The session's todo list |
| `/rules [rm N]` | Your standing rules; `rm` revokes one |
| `/approvals` | Calls in this session waiting for a decision |
| `/plan` | The last approved plan |
| `/team` | Team members |
| `/session` | Session id, user, workspace, mode and event count |
| `/skills` | Skills in the workspace's skill directories, `.adk/skills/` and `~/.adk/skills/` |
| `/cost` | Your spend: model calls, tokens and cost per acting agent and vendor |
| `/memory` | The harness's file memory, when available |
| `/help`, `exit` | Lists the commands; leaves the REPL |

Opening a session that has held calls with decisions resumes them first.

## Sessions and `harness.db`

Sessions, held calls, durable runs, modes and standing rules live in one SQLite file,
`<config dir>/adk-rust/harness.db`, created readable by you only. Sessions belong to the OS
user and the app `adk-rust`. A session records the workspace it started in and refuses to
continue from another `--dir`. `--ephemeral` keeps everything in memory.

```bash
adk-rust sessions list
adk-rust sessions show 6754153c-…
adk-rust rules add bash --prefix "cargo test"        # allow `cargo test …` everywhere
adk-rust rules add bash --prefix "git push" --deny "push from the release branch only"
adk-rust rules list
adk-rust rules rm 2
```

`rules add --operator --source mcp:github list_issues` pins any declaration of a publisher's
tool rather than the current one, for tools you maintain yourself.

## Goals

```bash
adk-rust goal "migrate to edition 2024" --until "cargo test" --max-iters 8
adk-rust goal "migrate to edition 2024" --until "cargo test" --resume
```

Each iteration is a harness turn in one session; after it the CLI runs `--until` in the
workspace with the same environment as `bash`. The goal's state is kept in the session
(`harness:goal`), so `--resume` continues the latest unfinished goal in this workspace with
its whole conversation. Goals need ask or auto mode. Exit codes: `0` met, `1` budget
exhausted, `3` calls held.

## Embedding the REPL

`adk_cli::harness::HarnessLauncher` runs any `HarnessAgent` the way `adk-rust` does:

```rust
use std::sync::Arc;

use adk_cli::console::ConsoleIo;
use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
use adk_harness::HarnessAgent;

async fn run(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    let harness = HarnessAgent::builder("my-harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .prompter(Arc::new(ConsolePrompter::new(ConsoleIo::terminal()?)))
        .build()?;
    // `cargo run` is the REPL; `cargo run -- serve` serves it on loopback.
    HarnessLauncher::new(harness).run().await
}
```

`HarnessLauncher::run_once` runs one turn and returns the calls held for a decision, for
scripts and tests.

## Related

- [Serving the harness](serving.md)
- [Launcher](../deployment/launcher.md)
- Spec: `docs/design/spec-3.1-harness-agent.md` §8.1
