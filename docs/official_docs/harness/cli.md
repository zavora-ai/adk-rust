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
| `adk-rust goal "goal" --until CMD` | Runs the harness goal engine until `CMD` exits 0, as a durable run in `harness.db`; `--resume` continues the goal in its session |
| `adk-rust ultracode "task"` | Implements in your mode, reviews with three read-only reviewers in parallel, revises until they approve |
| `adk-rust serve` | The harness in plan mode behind the HTTP server with the approval inbox; see [Serving the harness](serving.md) |
| `adk-rust sessions list\|show\|rm` | Your harness sessions |
| `adk-rust rules list\|add\|rm` | Your standing rules |
| `adk-rust memory list\|show\|forget\|accept\|export\|import` | The long-term facts the harness learned; see [Long-term memory](#long-term-memory) |
| `adk-rust approvals list\|approve\|deny` | Calls waiting for a decision, here or on a served harness with `--server URL --token T` |
| `adk-rust store status\|rotate-key` | The key `harness.db` is sealed under and which stores are wrapped; rotation to a new key (spec 3.1 §7.12); see [The encrypted store](#the-encrypted-store) |

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
| `--no-memory` | Turns long-term memory off: nothing is recalled or learned |
| `--effort low\|medium\|high` | The reasoning effort of this session's model clients (spec 3.1 §7.23); without it the `[effort]` policy applies per mode: plan high, everything else medium |
| `--store-key keychain:<entry>\|secret:<provider>/<name>\|env:<VAR>` | Where the key sealing `harness.db` is kept (spec 3.1 §7.12); without it `[store] key` in the user config, else the store is plaintext |

> **Note:** packs report themselves unavailable until their part of spec 3.1 lands (§7.16).

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

[effort]                     # spec 3.1 §7.23; every key optional
plan = "high"
ask = "medium"
members = "low"
distiller = "low"

[store]                      # the user config only (spec 3.1 §7.12)
key = "keychain:adk-rust"
```

`[effort]` names a level per mode (`read_only`, `plan`, `ask`, `auto`) and per role
(`members`, `distiller`, `sampling`); a project file may set it, since effort costs money,
not permissions. `[store]` names where the store key lives and is read from the user config
only; a project file setting it is ignored with a note.

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
in the session and removes it from `/approvals`, so nothing you have not seen runs under the
looser mode. Standing rules
apply where the mode would ask.

## The REPL

The banner shows the workspace, mode, shell isolation, team members and session id. The
first time a workspace has instruction files (`AGENTS.md`, or a fallback name without one), the
REPL lists them with their sizes and hashes and asks whether to trust them; untrusted files
are listed to the model but not loaded. A trust decision covers the files' exact content:
after an instruction file is edited, added or removed, the files are not loaded and the REPL
asks again.

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
| `/memory` | Working notes and long-term facts with their status, kind, source and session; `/memory forget ID` and `/memory accept ID` act on one fact (a unique prefix of the id is enough) |
| `/effort [low\|medium\|high]` | Shows the session's reasoning effort and the policy; with a level, restarts the session's model clients at it (spec 3.1 §7.23) |
| `/help`, `exit` | Lists the commands; leaves the REPL |

Opening a session that has held calls with decisions resumes them first.

## Sessions and `harness.db`

Sessions, held calls, durable runs, modes, standing rules and long-term facts live in one
SQLite file,
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

## The encrypted store

With a store key, every payload in `harness.db` is sealed before it reaches the file
(spec 3.1 §7.12, §9.2 I57): session state and events through `EncryptedSession`, held calls
and decisions through `EncryptedApprovalStore`, and the mode, rule, shell-ledger, goal and
fact stores through the same `adk-enc:v1:` AES-256-GCM envelope, each ciphertext bound to
its row. Identifiers, timestamps and status columns stay plaintext for indexing. A database
written under a key refuses to open without it or with another, and never falls back to
plaintext; the key never enters a prompt, a session or the audit log.

| Source | Form | Rotation |
|---|---|---|
| OS keychain | `keychain:<entry>` (`keychain:adk-rust`) | `rotate-key` stores the new keyring in the entry |
| Secret provider | `secret:gcp/<name>` (`GOOGLE_CLOUD_PROJECT` set), `secret:aws/<name>` (features `secrets-gcp`, `secrets-aws`) | `rotate-key` prints the new keyring document to store in the secret |
| Environment | `env:<VAR>`, holding a base64 32-byte key or a keyring document | `rotate-key` prints the new keyring document to export |

The first run offers to create a key in the OS keychain and keeps the choice as
`[store] key = "keychain:adk-rust"` in the user config. `adk-rust store status` reports the
key source, the key the database is sealed under, and per store how many rows are
envelopes. `adk-rust store rotate-key` re-seals every store under a new key, retires the
current one, seals rows written before any key was configured, vacuums the file so freed
pages carry no plaintext, and reports the `harness.store.key_rotated` record (key ids,
stores, rows, duration).

```bash
adk-rust --store-key keychain:adk-rust       # creates the key on first use
adk-rust store status
adk-rust store rotate-key                    # a generated key; --key <base64> for your own
ADK_STORE_KEY=$(openssl rand -base64 32) adk-rust --store-key env:ADK_STORE_KEY
```

## Model posture

Each session's model clients are built at the reasoning effort of its mode (spec 3.1
§7.23): Gemini `thinking_level`, OpenAI `reasoning_effort`, Anthropic `output_config.effort`
with adaptive thinking; Ollama and mistral.rs have no such control and run unchanged. The
member model is built at `members` and the distiller at `distiller`. Until the request-level
effort of spec 3.1 §10 C4 lands, effort changes per session, not per mode switch: `--effort`
and `[effort]` set it, and `/effort` restarts the session's clients. The banner shows the
level. Caching is on where the provider supports it — Anthropic and OpenAI cache the prefix
on their own, a Gemini model gets the runner's explicit context cache — and `/cost` shows
cached against uncached prompt tokens per turn.

## Local posture

`--provider ollama` (compiled in by default) and `--provider mistralrs --model <Hugging Face
id or .gguf path>` (feature `mistralrs`, a heavy build; GPU through `adk-mistralrs`'s `cuda`
or `metal`) run the model on this machine (spec 3.1 §8.1, H10). With a local provider,
long-term memory recalls with a local ONNX embedder (`BAAI/bge-small-en-v1.5`, fetched
once into the Hugging Face hub cache, feature `memory-local`), so with `--sandbox` and
`--store-key` nothing leaves the machine but the local model endpoint.

```bash
adk-rust --provider ollama --model qwen3.5 --sandbox --store-key keychain:adk-rust
```

## Long-term memory

The REPL, `code` and `goal` keep long-term facts in `harness.db` (spec 3.1 §7.15.2): the
preferences you state, how a project builds and tests, what fixed a failure. When a session
ends — `exit` in the REPL, the end of `code` or `goal` — the session's model distils it into
facts; later sessions in the same project recall them on each turn. The first run with
memory says once what is stored and where. `serve` and `--ephemeral` keep no facts, and
`--no-memory` turns memory off.

| Status | Means |
|---|---|
| `known` | You said it (your own words are quoted in the fact's provenance), or you confirmed it |
| `assumed` | The agent inferred it, or read it in a command's output or a file; a hint until you confirm it |
| `superseded`, `forgotten` | History: replaced by a newer fact, or forgotten; never recalled |

Recall finds related facts with an embedder when one is available — the model's provider
when it embeds (Gemini, OpenAI), else Gemini or OpenAI when its key is set — and by shared
words otherwise, with nothing leaving the machine for it. The banner shows
`Memory : long-term facts on`.

```bash
adk-rust memory list                     # this project's facts and your preferences, known first
adk-rust memory list --all               # every project, with superseded and forgotten facts
adk-rust memory show 01M5D9              # one fact with its provenance, by id or a unique prefix
adk-rust memory accept 01M5D9            # confirm an assumed fact: it becomes known
adk-rust memory forget 01M5D9            # never recalled again; kept as history
adk-rust memory export --out facts.jsonl # every fact, as JSON lines with provenance
adk-rust memory import facts.jsonl       # imported facts arrive assumed
```

## Goals

```bash
adk-rust goal "migrate to edition 2024" --until "cargo test" --max-iters 8 --check-timeout 900
adk-rust goal "migrate to edition 2024" --until "cargo test" --resume
```

`adk-rust goal` starts the goal with `Harness::start_goal` ([Plan mode and goals](plan-and-goals.md)).
Each iteration is a harness turn followed by `--until`, which runs through the harness shell
in the workspace for up to `--check-timeout` seconds (600 by default); a check that runs out
of time counts as failed. The goal is a durable run in `harness.db` on its own queue
(`harness.goal`), executed by a worker inside the command that claims nothing else, so
`adk-rust serve` can keep background runs in the same file. The goal's state lives in the
harness goal store, mirrored to the session as `harness:goal`.

| Situation | What happens |
|---|---|
| A call is held (no terminal and no `--yes`) | The run parks and the command exits `3`. Approve with `adk-rust approvals approve ID`, which prints the `goal … --resume` command; `--resume` continues the run at the held call, which runs once |
| `--resume` before the call is decided | Nothing runs; the command lists the held call and exits `3` again |
| The budget runs out | Exit `1`; `--resume --max-iters N`, with `N` above the iterations used, continues the goal |
| Ctrl-C, or the process stops | Exit `130` on Ctrl-C; `--resume` continues the goal in its session, reclaiming the stopped process's run once its 10-second lease lapses |

`--resume` continues the latest unfinished goal in this workspace, in its session and with
its whole conversation. Goals need ask or auto mode. Exit codes: `0` met, `1` budget
exhausted, `3` calls held, `130` interrupted. With `--ephemeral` the goal runs in the
command's process only and cannot be resumed.

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

## Logging

Every harness command installs a `tracing` subscriber on stderr. `RUST_LOG` sets the filter
(`RUST_LOG=adk_harness=debug adk-rust code "..."`); without it only `warn` and `error` events
print, so a failed session-end distillation, a stale standing rule or a refused sandbox is seen.
A failed distillation is also reported in one line when the command ends:

```text
note: long-term memory was not updated for session s-01: model.openai.unavailable (...)
```

`Harness::close_with_report` returns the same failures to an embedding program.

## Related

- [Serving the harness](serving.md)
- [Launcher](../deployment/launcher.md)
- Spec: `docs/design/spec-3.1-harness-agent.md` §8.1
