# The harness in your terminal

`adk-rust` with no subcommand is the harness: the governed coding agent of spec 3.1
(`adk-harness`) in the current directory. It reads and edits files confined to the workspace,
runs commands with `bash` behind per-call approval, keeps a todo list, follows the
workspace's `AGENTS.md` once you trust it, and remembers the decisions you ask it to.

```bash
cargo install adk-cli          # default features include `harness` and `persist`
cd my-project
adk-rust                       # the harness in the full-screen terminal, ask mode
adk-rust --ui repl             # the harness REPL
```

> **Note:** the harness refuses to start in your home directory or `/` unless `--dir` names
> them explicitly, since the agent could read and change everything under them.

## Commands

| Invocation | Behaviour |
|---|---|
| `adk-rust` | The harness in `--dir` (default `.`), ask mode, in the [full-screen terminal](../surfaces/terminal.md) on a capable terminal; `--ui repl` for the REPL, approvals in the terminal |
| `adk-rust chat` | The plain chat agent, without workspace tools |
| `adk-rust code "task"` | One harness turn; exit code `3` means calls are held for a decision (the session id is printed) |
| `adk-rust goal "goal" --until CMD` | Runs the harness goal engine until `CMD` exits 0, as a durable run in `harness.db`; `--resume` continues the goal in its session |
| `adk-rust ultracode "task"` | Implements in your mode, routes the review to parallel read-only reviewers or a read-only Blackboard board, revises until they approve; see [Ultracode](#ultracode) |
| `adk-rust serve` | The harness in plan mode behind the HTTP server with the approval inbox; see [Serving the harness](serving.md) |
| `adk-rust sessions list\|show\|rm` | Your harness sessions |
| `adk-rust rules list\|add\|rm` | Your standing rules |
| `adk-rust memory list\|show\|forget\|accept\|export\|import\|undo` | The long-term facts the harness learned; see [Long-term memory](#long-term-memory) |
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
| `--pack browser\|computer-use\|payments` | Adds a pack: `browser` installs the browser pack over the WebDriver at `ADK_WEBDRIVER_URL` (default `http://localhost:4444`; adk-cli feature `browser`), see [Browser and computer use](packs.md); `computer-use` and `payments` name the library API |
| `--member NAME=explore\|general` | Adds a team member |
| `--no-memory` | Turns long-term memory off: nothing is recalled or learned |
| `--no-consolidation` | Turns the kernel's consolidation tick off under `adk-rust up` ([Long-term memory](memory.md#consolidation)) |
| `--effort low\|medium\|high` | The reasoning effort of every model call in this session, whatever the mode or role (spec 3.1 §7.23); without it the `[effort]` policy applies per mode: plan high, members low, everything else medium |
| `--store-key keychain:<entry>\|secret:<provider>/<name>\|env:<VAR>` | Where the key sealing `harness.db` is kept (spec 3.1 §7.12); without it `[store] key` in the user config, else the store is plaintext |
| `--no-hooks` | Runs no hooks, the workspace's included (spec 3.1 §7.22); `/hooks` lists the effective set |
| `--web` / `--no-web` | Declares the web tools (on by default) or none at all (spec 3.1 §7.20) |
| `--allow-domain HOST` | A host `web_fetch` reads without approval: exact, or `*.example.com`; repeatable |
| `--search provider\|off\|searxng\|deepseek` | Where web search runs (spec 3.1 §7.20): `provider` (the default) is the built-in search of the provider the model's client serves; `searxng` and `deepseek` replace it with the governed `web_search` over a SearXNG instance (`[web] searxng_url`, else `SEARXNG_URL`) or DeepSeek's Search API (`DEEPSEEK_API_KEY`); `off` declares none. The banner's `Search` line and `/web` name the path |
| `--preset coding\|personal` | Applies a preset (spec 3.3 §10 H1): `personal` works in `<home>/workspace` instead of the current directory, offers no host shell, and asks for quiet time and a daily spending limit at its first run; `coding` is the coding agent's prompt and no members. See [The personal preset and take over](personal.md) |
| `--checkpointer URL` | Where plans and goal graphs are checkpointed: `sqlite://`, `postgres://`, `redis://`, `mongodb://`, `neo4j://` or `firestore://` (spec 3.1 §7.11); without it `[plan] checkpointer` in the user config, else the store the sessions are in. Not with `--ephemeral` |

> **Note:** `--pack computer-use` and `--pack payments` refuse at start: a desktop runtime and a merchant service are connections only the library API takes (spec 3.1 §7.16, §7.24).

`--pack payments` is refused with the reason: the pack needs a merchant checkout service, a
transaction store and a mandate, which no flag or config file can supply; see
[Payments](payments.md#in-the-cli) for the binary that does.

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

[web]                        # spec 3.1 §7.20; `web = false` turns the web tools off instead
search = "searxng"           # provider (default), off, searxng or deepseek
searxng_url = "http://localhost:8888"

[ultracode]                  # spec 3.1 §7.10; see Ultracode
max_rounds = 2

[plan]                       # the user config only (spec 3.1 §7.11)
checkpointer = "postgres://plans@db/harness"

[[hooks]]                    # spec 3.1 §7.22; see Hooks
event = "post_edit"
command = "cargo fmt"
```

`[effort]` names a level per mode (`read_only`, `plan`, `ask`, `auto`) and per role
(`members`, `distiller`, `sampling`); a project file may set it, since effort costs money,
not permissions. `[store]` names where the store key lives and is read from the user config
only; a project file setting it is ignored with a note. `[web]` names where web search runs:
a project file may set `search = "off"`, and any other `search` or a `searxng_url` there is
ignored with a note, since it would choose where the agent's queries go. `[plan] checkpointer` is read from the
user config only: a checkpointer location is not a permission, but a workspace file naming one would send
plan state to a server the repository chose, so the flag wins, then the user config, then
the store the sessions are in. `[[hooks]]` tables are hooks on
tool events ([Hooks](hooks.md#in-the-cli)): the user config's run before the workspace's,
and the workspace's run only once the workspace is trusted.

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
| `/effort [low\|medium\|high]` | Shows the session's reasoning effort and the policy; with a level, sets every mode and role to it from the next model call (spec 3.1 §7.23) |
| `/hooks` | The hooks on tool events — each with its source, isolation, timeout and whether it runs — and the bounds of one turn (spec 3.1 §7.22); a workspace hook runs only under the workspace's trust |
| `/help`, `exit` | Lists the commands; leaves the REPL |

A slash line that names no command goes to the agent as typed, after one dim line on the
console: `no command /aprovals: sent to the agent`. The model knows every command, subcommand
and flag of `adk-rust` from the `<harness>` block and `harness_help`, so it names the command
you meant (see [What the harness knows about itself](self-knowledge.md)).

Opening a session that has held calls with decisions resumes them first.

Tab completes a slash command at the start of a line and an `@server:uri` mention
anywhere in it, from the resources the MCP servers publish (spec 3.1 §7.21); the mentions
are refreshed after each turn and after `/mcp resources SERVER`. A note from an `on_hold`
or `on_stop` hook is shown as `[hook:source] text` after the call or the turn; the model
never sees it. A decision printed without asking — `[approved by --yes] …`, or the denial
of a call no terminal could ask about — follows the `[tool-call]` line it belongs to,
including under a durable goal, where the console reads the run's events as the worker
records them.

## Sessions and `harness.db`

Sessions, held calls, durable runs, modes, standing rules, long-term facts and plan
checkpoints live in one SQLite file,
`<config dir>/adk-rust/harness.db`, created readable by you only. An approved plan therefore
runs in the next process, and the banner's `Plans` line, or the `plans` line of `code` and
`goal`, says where plans are kept. `serve --store` moves them with the other stores; see
[Running replicas](serving.md#running-replicas). Sessions belong to the OS
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
tool rather than the current one, for tools you maintain yourself. `--store <url>` manages
the rules of served replicas that share that store instead of `harness.db`. `rules add` and
`rules rm` go through the harness, which records `harness.rule.created` or
`harness.rule.revoked` with `surface: cli` before the store write, as the REST routes record
theirs with `surface: rest`; a change whose record cannot be written is not made.

## The encrypted store

With a store key, every payload in `harness.db` is sealed before it reaches the file
(spec 3.1 §7.12, §9.2 I57): session state and events through `EncryptedSession`, held calls
and decisions through `EncryptedApprovalStore`, and the mode, rule, shell-ledger, goal and
fact stores and plan checkpoints through the same `adk-enc:v1:` AES-256-GCM envelope, each
ciphertext bound to its row; a plan checkpointer named by `--checkpointer` is sealed too. Identifiers, timestamps and status columns stay plaintext for indexing. A database
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

Every model call carries the reasoning effort of the mode it runs in (spec 3.1 §7.23):
the harness sets it on each request, so approving a plan or switching with `/mode` changes
the next call's effort on the same client. Members run at `members` and the distiller at
`distiller`. The provider maps the level to its own control — Gemini `thinking_level`,
OpenAI `reasoning_effort`, Anthropic `output_config.effort` with adaptive thinking — and a
model without one (Ollama, mistral.rs, Claude Haiku 4.5) runs unchanged. `--effort` and
`[effort]` set the policy, `/effort low|medium|high` sets every mode and role to one level
from the next call, and the banner shows the current level and the policy. Caching is on
where the provider supports it — Anthropic and OpenAI cache the prefix on their own, a
Gemini model gets the runner's explicit context cache — and `/cost` shows cached against
uncached prompt tokens per turn.

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
ends — leaving the terminal or `exit` in the REPL, the end of `code` or `goal` — the session's
model distils it into facts; later sessions in the same project recall them on each turn. The
first run with memory says once what is stored and where. `serve` and `--ephemeral` keep no
facts, and `--no-memory` turns memory off.

A session that ends without that — the process killed, the terminal window closed, a signal
— is distilled at the next start in the same workspace (spec 3.1 §7.33 fix 9). In the
background, the start distils the five most recent of your sessions there whose distillation
stops short of their last event, each under the session-end distillation's budget, and says
what they added:

```text
2 facts from 1 earlier session that ended without saving were added as assumed; /memory lists them
```

| Rule | Why |
|---|---|
| The facts are `assumed` | Distillation after the fact runs with verified-quote promotion off, as the kernel's consolidation tick does; `adk-rust memory accept ID` confirms one |
| A session active in the last ten minutes waits for a later start | Another terminal may still be working in it, and distils it itself |
| A session in `read-only` or `plan` mode teaches nothing | As at the end of any session |
| The terminal shows the line when it arrives; `--ui repl` prints it at the next prompt | The line console cannot print over the line you are typing |
| Attached to a running kernel, nothing is caught up at start | The kernel's [consolidation tick](memory.md#consolidation) distils the sessions it missed |

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
adk-rust memory list --source consolidation  # the consolidation ticks, and the facts they wrote
adk-rust memory undo 199b579a            # revert one tick's status changes
```

Under `adk-rust up` in ask or auto mode, the kernel's consolidation tick distils the sessions
the per-session distiller never reached and tidies assumed facts (see
[Consolidation](memory.md#consolidation)); `memory list --source consolidation` shows each
tick, `unfinished` while it runs, and the facts it wrote; `memory undo TICK` makes what a
finished tick superseded or forgot assumed again and forgets what it wrote; and
`--no-consolidation` turns the tick off.

A tick after `adk-rust up` restarted from a hard kill distils the session the kill cut off.
Its fact repeats one the turn remembered, and the tick supersedes the older one. `memory undo`
makes the superseded fact assumed again and forgets the one the tick wrote:

```text
$ adk-rust memory list --source consolidation
tick bcf8b12e-ed34-4c37-b6a6-c055c58ac795 2026-10-23 15:21 · 1 superseded · 0 forgotten · 0 merged · 1 distilled

01M5MNNJ8B8N09ADVX8CP3WP6P assumed · project_fact · 2026-10-23 · agent_inferred in session tick-live-1 · project
    The project exclusively uses pnpm instead of npm, and tests are executed using pnpm test.
$ adk-rust memory list --all
01M5MNNJ8B8N09ADVX8CP3WP6P assumed · project_fact · 2026-10-23 · agent_inferred in session tick-live-1 · project
    The project exclusively uses pnpm instead of npm, and tests are executed using pnpm test.
01M5MN18T9860AKG9X4Z0SV90R superseded · project_fact · 2026-10-23 · agent_inferred in session tick-live-1 · project
    we always use pnpm, never npm, and the tests run with pnpm test
$ adk-rust memory undo bcf8b12e
undid tick bcf8b12e-ed34-4c37-b6a6-c055c58ac795: 1 fact(s) assumed again, 1 it wrote forgotten, 0 kept (changed since)
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

## Ultracode

```bash
adk-rust ultracode "make session tokens expire after an hour" --mode auto --receipt receipt.json
```

`adk-rust ultracode` implements the task in one turn in your mode, through the same
launcher, prompter and governed path as `code`. It then routes the review once, on the
change that turn made, and keeps that plan for the run (spec 3.1 §7.10):

| Architecture | A review stage | Bound per stage |
|---|---|---|
| `graph` | One fresh read-only, shell-less harness per reviewer, in parallel | `reviewer_model_calls` per reviewer |
| `blackboard` | One round-robin board of read-only members of a read-only, shell-less harness; each seat reads what the others said, called and got back; `boardRounds` rounds | `board_model_calls` |

Every model call of the run takes its effort from the session's policy (`--effort` or
`[effort]`, spec 3.1 §7.23): the implementer its mode's level, an independent reviewer
`read_only`, a board seat and the router `members`.

After each stage the verdicts are synthesized: every reviewer approving finishes the run;
otherwise the implementer revises with the notes and the next stage reviews again, up to
`max_rounds` stages. A reviewer's verdict is its last `VERDICT` line, and a line naming both
`approve` and `changes` requests changes. A reviewer with no `VERDICT` line, a failed reviewer
and a board seat that never spoke are not approvals. The notes reach the implementer inside an
`<untrusted-content source="review">` block, as findings to check rather than instructions.

A revise can change more than the first turn did. After each revise the whole change since
the run began is checked again: when it is now sensitive and the roster has no `security`
seat, the stages that follow add one, and `harness.ultracode.escalated` records why before
the next stage starts. The architecture and the rounds stay as routed:

```text
━━ escalate ━━ a sensitive path changed: src/auth/token.rs; reviewers now correctness, edge-cases, security, style
```

### Routing

The first rule that matches decides (`ultracode-route/1`):

| Rule | When | Plan | Model calls |
|---|---|---|---|
| `override` | `--architecture graph\|blackboard`, or `architecture` in a config file | That architecture | 0 |
| `unchanged` | The first turn held, failed, was interrupted or changed no file | `graph` over correctness, edge-cases and style | 0 |
| `sensitive` | A changed path is protected, has a security-vocabulary component (`auth`, `crypto`, `security`, `token`, `secret`, `policy`, `sandbox`, …) or matches `sensitive_paths` in your user config; or the workspace has more than 20,000 files, so the change could not be seen whole | `blackboard` with a `security` seat | 0 |
| `model` | Router mode `model` (the default) | The model's answer to a closed schema at the `members` effort, validated whole | 1: no tools, 4,000 tokens, 20 seconds |
| `heuristic` | Router mode `heuristic`, or the model call failed or answered anything invalid | A score over the signals | 0 |

| Floor or limit | Value |
|---|---|
| Roster | Always correctness, edge-cases and style; a sensitive change, including a match of `sensitive_paths` in `.adk/harness.toml`, adds `security`, in style's seat when `max_reviewers` is 3 and otherwise ahead of the lowest-priority specialist when the roster is full |
| What the router chooses | The architecture, up to `max_reviewers − 3` specialists (`security`, `performance`, `tests`, `api-compat`, `concurrency`) and the board rounds; never the review rounds |
| Signals | Keywords of the task, and file paths, sizes and modification times before and after the first turn; no file contents, no `git` process. Of `.git`, only `config`, `config.worktree`, `hooks/` and `info/` are seen; build and dependency directories (`target`, `node_modules`, `dist`, `build`, `.venv`, …) only when a `sensitive_paths` glob names them from the workspace root, such as `dist/**` |
| The model's rationale | Printed after `why:` and recorded; never part of any agent's prompt |

A router answer is at most 4 KiB, one JSON object (a code fence is accepted), and applies
whole or not at all; the fallback is recorded with one of `router.call_failed`,
`router.timeout`, `router.budget_exceeded`, `router.unparseable`, `router.schema_invalid` or
`router.over_limit`:

```text
route: graph (heuristic: router.unparseable) · reviewers correctness, edge-cases, style · board rounds 1 · review rounds 2
why: heuristic score 2 (packages>=2)
```

### Flags

| Flag | Effect |
|---|---|
| `--architecture auto\|graph\|blackboard` | `auto` (the default) routes; the others fix the architecture |
| `--router model\|heuristic` | How `auto` chooses; `model` is the default |
| `--max-rounds N` | Review stages, 1 to 5; default 2 |
| `--max-model-calls N` | The whole run's model calls, up to 2,000; default 400 |
| `--receipt FILE` | Writes the run's `harness.ultracode.*` records, the verification of the run's signed audit log and the router's raw answer. The file is written as you, outside any sandbox, so the write is refused when a link inside the workspace is on the way to it or its directory resolves elsewhere than when the run started |

### Budget

Every step has its own `RunBudget`: an implement or revise turn `implement_model_calls`, an
independent reviewer `reviewer_model_calls`, a board `board_model_calls`, the router one
call. A review stage starts only when what remains of `max_model_calls` covers it, and a
revise only when it covers the revise and the next stage; otherwise the run ends with the
decision `budgetExhausted`. At start, one implement turn, the router and three reviewers must
fit in `max_model_calls`, or the command says what to change.

### The `[ultracode]` table

| Key | Default | Ceiling | In `.adk/harness.toml` |
|---|---|---|---|
| `architecture` | `auto` | — | `graph` only |
| `router` | `model` | — | `heuristic` only |
| `max_rounds`, `max_reviewers`, `max_board_rounds` | 2, 4, 2 | 5, 6, 3 | Lower only; floors 1, 3, 1 |
| `implement_model_calls`, `reviewer_model_calls`, `board_model_calls` | 100, 20, 48 | 500, 50, 150 | Lower only |
| `max_model_calls` | 400 | 2,000 | Lower only |
| `sensitive_paths` | `[]` | — | Append only; a match adds the `security` seat but never chooses the board |

In the user config a value beyond a ceiling or floor is clamped, with a note. Flags win, and
a flag beyond a ceiling is an error that names it.

### Records and the receipt

One signed audit log holds the run: the implementer's, reviewers' and board's governed calls
and these records, kind `harness`, actor `urn:adk:agent:adk-rust/ultracode`:

| Record | Written |
|---|---|
| `harness.ultracode.started` | Before the first turn: the limits, the requested architecture and its source, the worst case and the bound |
| `harness.ultracode.routed` | Before the first review: the rule, the plan, the inputs, the heuristic's trace, the router call by digest and its validated answer |
| `harness.ultracode.escalated` | Before the stage after a revise that made the change sensitive: the change since the run began, the plan it tightened and the plan with the `security` seat |
| `harness.ultracode.reviewed` | After each stage: the verdicts, the board's status and fallback seats, the stage's model calls, whether the workspace is unchanged |
| `harness.ultracode.finished` | At the end: rounds, decision, whether the review escalated, model calls, held calls |

`adk_cli::harness::ultracode::replay(&receipt["routed"])` recomputes the plan from the
recorded inputs and fails when it differs; `replay_escalation(&receipt["routed"],
&receipt["escalated"])` does the same for an escalation. Model calls of a turn that a
denial or Ctrl-C ended count against `max_model_calls` like any other. Exit codes: `0`, or `3` when calls are held.

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
