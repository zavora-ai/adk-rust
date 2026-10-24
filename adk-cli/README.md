# adk-cli

Command-line launcher for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-cli.svg)](https://crates.io/crates/adk-cli)
[![Documentation](https://docs.rs/adk-cli/badge.svg)](https://docs.rs/adk-cli)
[![License](https://img.shields.io/crates/l/adk-cli.svg)](../LICENSE)

## Overview

`adk-cli` provides two things:

- **`adk-rust` binary** — the governed **harness** (spec 3.1) in your terminal and behind the
  server: a coding agent that edits files in the workspace and runs commands after you
  approve them (`adk-rust`, `code`, `goal`, `ultracode`, `serve`), with remembered approvals
  (`rules`), held calls (`approvals`) and sessions (`sessions`) in one SQLite file; the
  harness as the always-on **agent kernel** (spec 3.2: `up`, `attach`, `ps`, `top`, `kill`,
  `suspend`, `resume`, `freeze`, `unfreeze`, `events`, `proc`, `partitions`); plus a plain chat agent
  (6 providers), skills, graph checkpoints, signed audit logs, and deploy
- **`Launcher` library** — embed a REPL and web server into any custom agent binary;
  `HarnessLauncher` does the same for a `HarnessAgent`

## Quick Start

```bash
cargo install adk-cli

# The harness REPL in the current project — interactive setup picks your provider on first run.
# File tools are confined to the directory; edits and `bash` (on the host) wait for approval.
cd my-project && adk-rust

# Or pre-configure a provider (the default build compiles Gemini, OpenAI and Anthropic;
# `cargo install adk-cli --features all-providers` adds the others)
adk-rust --provider gemini --api-key your-key

# Harness sessions are kept on disk: continue the last one in this workspace, or a named one
adk-rust --continue
adk-rust --session release-notes --mode plan

# One task, a goal, or an implement-and-review loop on the harness
adk-rust code "make the failing test pass"
adk-rust goal "all tests green" --until "cargo test" --resume
adk-rust ultracode "add input validation"

# The harness behind the HTTP server, in plan mode, with the approval inbox
adk-rust serve --port 3000

# The harness as the agent kernel: its turns, goals and schedules run as durable processes,
# and every terminal attaches to the same running state
adk-rust up --mode ask
adk-rust ps                      # from another terminal
adk-rust attach                  # a REPL whose turns the kernel runs

# The plain chat agent
adk-rust chat
```

See [The harness in your terminal](../docs/official_docs/harness/cli.md),
[Serving the harness](../docs/official_docs/harness/serving.md) and
[Kernel commands](../docs/official_docs/kernel/cli.md).

## Feature Flags

| Feature | Default | Enables |
|---------|---------|---------|
| `gemini`, `openai`, `anthropic` | yes | The Gemini, OpenAI and Anthropic providers |
| `audit` | yes | `adk-rust audit verify` and `audit keygen` (through `adk-auth`'s `signed-audit`) |
| `persist` | yes | Sessions, held calls, durable runs, modes and standing rules in SQLite at `adk-rust/harness.db` (through `adk-session`'s and `adk-harness`'s `sqlite`) |
| `surface` | yes | The web home of `adk-surface` at `/ui/` on `serve` and `up`, over the harness (spec 3.3 §5.1, §10 S1), with `harness` |
| `harness` | yes | The harness: `adk-rust` REPL, `code`, `goal`, `ultracode` and `serve` on `adk-harness`, `sessions`, `rules`, `approvals`, and the `adk_cli::harness` module; the agent kernel's `up`, `attach`, `ps`, `top`, `kill`, `suspend`, `resume`, `freeze`, `unfreeze`, `events`, `proc` and `partitions` on `adk-kernel`, with its daily baselines (`proc-baselines`) |
| `deepseek`, `groq`, `ollama` | no | The other providers |
| `all-providers` | no | Every provider |
| `audit-sql` | no | `adk-rust audit export` from SQLite or PostgreSQL audit stores |
| `graph-sqlite` | no | The SQLite checkpointer for `adk-rust graph` |
| `gcp-deploy` | no | `adk-rust deploy agent-engine` |
| `vertex-agent-registry` | no | `adk-rust registry` |
| `vertex-skill-registry` | no | Skill Registry lookups in `adk-rust skills` |
| `telemetry-otlp` | no | OTLP export for `TelemetryConfig::Otlp` |
| `optimize` | no | The `Launcher` CLI's `optimize` subcommand, which parses its arguments and logs that optimization is wired through `adk-eval`'s `PromptOptimizer`; it optimizes nothing itself |
| `tui` | no | `adk-rust` with no subcommand as the terminal surface (spec 3.3 §5.2, `adk-surface`'s `tui`): full screen on a capable terminal, `--ui inline`, `--output json` with exit codes, `--theme`, and `--server` with `--pair` for a kernel on another machine; see [the terminal](../docs/official_docs/surfaces/terminal.md) |
| `channels` | yes | `adk-rust up` starts the Telegram chat thread when `TELEGRAM_BOT_TOKEN` is set (spec 3.3 §5.3, `adk-surface`'s `channels`), with `harness` and `surface`; see [chat](../docs/official_docs/surfaces/chat.md) |
| `scripted-model` | no | Hidden `--model-script` and `--approval-fixture` flags for end-to-end tests: a scripted model and a `record_note` tool that needs approval |

`cargo install adk-cli` builds the default features; add `--features all-providers` for every provider.

## Supported Providers

| Provider | Flag | Default Model | Env Var |
|----------|------|---------------|---------|
| Gemini | `--provider gemini` | `gemini-3.7-flash` | `GOOGLE_API_KEY` / `GEMINI_API_KEY` |
| OpenAI | `--provider openai` | `gpt-5.6-terra` | `OPENAI_API_KEY` |
| Anthropic | `--provider anthropic` | `claude-sonnet-5` | `ANTHROPIC_API_KEY` |
| DeepSeek | `--provider deepseek` | `deepseek-flash` | `DEEPSEEK_API_KEY` |
| Groq | `--provider groq` | `openai/gpt-oss-120b` | `GROQ_API_KEY` |
| Ollama | `--provider ollama` | `qwen3.5` | _(none, local)_ |

## First-Run Setup

If no provider is configured, every agent command (`chat`, `serve`, `code`, `goal`,
`ultracode`) launches an interactive setup:

1. Choose a provider from the menu, which lists only the providers compiled into this build
2. Enter your API key (skipped for Ollama)
3. Provider and model are saved to `adk-rust/config.json` in the platform config directory (`~/.config` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows)
4. API keys are stored in your OS credential store (Keychain, Credential Manager, Secret Service)

On subsequent runs, the saved config is used automatically. CLI flags always
take priority over environment variables, secure credential storage, and saved config.

Setup prompts only on a terminal. Without one, a command with no configured provider, or
with a provider that has no key, fails with the flags and environment variables to set. A
saved or `--provider` choice that this build does not compile in fails with the
`cargo install adk-cli --features <provider>` hint.

## Binary Commands

```text
adk-rust              The harness REPL in the current directory (default)
adk-rust chat         Interactive REPL with the plain chat agent
adk-rust serve        Serve the harness (plan mode, approval inbox, the web home at /ui/); `--agent chat` serves chat
adk-rust code         One harness turn on a task; exit 3 when calls are held
adk-rust goal         Harness turns until a success-condition command exits 0
adk-rust ultracode    Implement, then route the review to parallel reviewers or a review board
adk-rust sessions     Harness sessions (list/show/rm)
adk-rust rules        Standing rules (list/add/rm)
adk-rust approvals    Held calls (list/approve/deny), locally or with --server
adk-rust up           The harness as the agent kernel, in the foreground
adk-rust attach       A REPL on the running kernel; `adk-rust` attaches when one runs
adk-rust ps / top     The kernel's processes; `top` by burn rate, refreshing
adk-rust kill / suspend / resume PID   Act on a process
adk-rust nice PID QUEUE                Move a queued or sleeping process to another queue
adk-rust agenda       Follow-ups proposed for later (list/approve/reject/schedule)
adk-rust wake         A sleeping process woken by a file change, a cron time or a webhook
adk-rust freeze / unfreeze             The kill switch, on every replica
adk-rust events       The kernel's event bus (`--follow`)
adk-rust proc         The kernel's activity ledger, baselines and observations
adk-rust partitions   The kernel's partitions (list/add/set-mode)
adk-rust pair         Pair a phone or terminal with a code (list/revoke; `accept` a chat account's code)
adk-rust skills       Skill tooling (list/validate/match)
adk-rust graph        Graph checkpoint inspection (steps/replay/fork/resume)
adk-rust audit        Signed audit log tools (verify/keygen; export with `audit-sql`)
adk-rust deploy       Deployment commands
adk-rust registry     Agent Registry registration and search (`vertex-agent-registry`)
```

### Global options (apply to `chat`, `serve`, `code`, `goal` and `ultracode`)

| Flag | Default | Description |
|------|---------|-------------|
| `--provider` | saved config or interactive | LLM provider |
| `--model` | provider default | Model name (provider-specific) |
| `--api-key` | secure store / env var | API key (overrides all other sources) |
| `--instruction` | built-in default | Agent system prompt; for `code`, `goal` and `ultracode`, project guidance added to the coding prompt |
| `--thinking-budget` | none | Enable provider-side thinking when supported |
| `--thinking-mode` | `auto` | Render emitted thinking: `auto`, `show`, `hide` |

### Harness options (the REPL, `code`, `goal`, `ultracode`, `serve`)

| Flag | Default | Description |
|------|---------|-------------|
| `--dir` | `.` | Workspace; the REPL refuses your home directory and `/` unless named |
| `--mode` | `ask` (`plan` for `serve`) | `read-only`, `plan`, `ask` or `auto` |
| `--yes` | off | Approve held calls without asking (never a policy denial, never an unsandboxed retry without `--allow-unsandboxed`) |
| `--session` / `--continue` / `--ephemeral` | new session | Continue a session, the latest one in this workspace, or keep nothing on disk |
| `--sandbox`, `--fail-if-unavailable` | off | Run `bash` in the OS sandbox; refuse to start without it |
| `--allow-unsandboxed` | off | Let `--yes` approve an unsandboxed retry |
| `--trust` | ask once | Trust the workspace's `AGENTS.md` without asking |
| `--pack`, `--member` | none | Packs and team members |
| `--no-hooks` | off | Run no hooks, the workspace's included; `/hooks` lists the effective set |
| `--preset` | none | `personal`: a workspace of notes and files under the home, no host shell, quiet time and a daily spending limit asked at first run (spec 3.3 §10 H1); `coding`: the coding agent's prompt |

Settings also come from `.adk/harness.toml` in the workspace (which can only tighten) and
`adk-rust/harness.toml` in the config directory, `[[hooks]]` tables included; flags win.
Tab completes slash commands and `@server:uri` mentions. See
[The harness in your terminal](../docs/official_docs/harness/cli.md).

### `adk-rust ultracode` options

`ultracode` implements in your mode, then routes the review once, on the change: parallel
independent reviewers (`graph`) or one round-robin board whose reviewers read each other
(`blackboard`), read-only and shell-less in both (spec 3.1 §7.10). A security-sensitive or
protected path routes to the board with a `security` seat; otherwise one closed-schema call
at the session's `members` effort chooses, with a deterministic heuristic when it fails. A revise that
makes the change sensitive adds the `security` seat to the stages after it.

| Flag | Default | Description |
|------|---------|-------------|
| `--architecture` | `auto` | `auto`, `graph` or `blackboard` |
| `--router` | `model` | How `auto` chooses: `model` or `heuristic` |
| `--max-rounds` | 2 | Review stages, at most 5 |
| `--max-model-calls` | 400 | The run's model calls, at most 2,000 |
| `--receipt FILE` | none | The run's `harness.ultracode.*` records, the audit-log verification and the router's raw answer |

The `[ultracode]` table in either config file sets the same limits and the per-step
budgets; `.adk/harness.toml` can only lower them. See
[Ultracode](../docs/official_docs/harness/cli.md#ultracode).

### `adk-rust chat` options

| Flag | Description |
|------|-------------|
| `--session <id>` | Continue the session with this id, creating it when it does not exist |
| `--continue` | Continue the most recent session |
| `--ephemeral` | Keep the conversation in memory only; nothing is written to disk |

With the `persist` feature, conversations are kept in `adk-rust/harness.db` in the platform
config directory, created readable by its owner only (mode 0600 on Unix, an owner-only DACL on
Windows), under the OS user name. The banner
shows the session id.

### `adk-rust serve` options

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `8080` | Server port |
| `--host` | `127.0.0.1` | Listen address; `0.0.0.0` listens on every interface and needs `--auth-token` |
| `--agent` | `harness` | `harness`, or `chat` for the plain agent as before |
| `--auth-token` | `$ADK_SERVE_TOKEN` | Bearer token callers present; without one only loopback peers are admitted |

The harness is served in plan mode with `/api/approvals`, `/api/runs` and its standing
rules at `/api/rules`; see [Serving the harness](../docs/official_docs/harness/serving.md).

The server answers only loopback names and IP addresses, and allows no cross-origin browser
access. The binary has no flag for more hosts or origins; a custom binary lists them with
`Launcher::with_security_config`.

### `adk-rust up` options

| Flag | Default | Description |
|------|---------|-------------|
| `--home` | `[kernel] home`, else the config directory's `adk-rust` | Kernel home: `kernel.sock`, `harness.db`, the replica id and its audit signing key |
| `--bind` | `[kernel] bind`, else `127.0.0.1:8080` | HTTP control plane and served harness; beyond loopback needs `--auth-token` |
| `--auth-token` | `$ADK_SERVE_TOKEN` | Bearer token HTTP callers present |
| `--store` | `harness.db` | `postgres://…` or `redis://…`: every store, the kernel's included, shared by replicas |

With `TELEGRAM_BOT_TOKEN` set, `up` also starts the Telegram chat thread: an account that
writes to the bot is paired with `adk-rust pair accept <code>`; see
[chat](../docs/official_docs/surfaces/chat.md#with-adk-rust-up).

The harness flags (`--mode`, `--dir`, `--sandbox`, …) apply as for `serve`. The other kernel
commands reach the kernel on its socket, or over HTTP with `--server URL [--token T]`; see
[Kernel commands](../docs/official_docs/kernel/cli.md) and
[The kernel control plane](../docs/official_docs/kernel/control-plane.md).

### `adk-rust audit` subcommands

```bash
adk-rust audit keygen audit.example.com --out audit             # writes audit.key and audit.vkey
adk-rust audit verify audit.jsonl --key "$(cat audit.vkey)"      # exits 1 when the log does not verify
adk-rust audit export --database-url sqlite://audit.db --log-id main --out audit.jsonl  # `audit-sql`
```

### `adk-rust skills` subcommands

```bash
adk-rust skills list                          # list indexed skills
adk-rust skills validate                      # validate .skills/ directory
adk-rust skills match --query "web scraping"  # rank skills by relevance
```

All skills commands accept `--json` for machine-readable output and `--path`
to specify the project root (defaults to `.`).

`skills match` additional options:

| Flag | Default | Description |
|------|---------|-------------|
| `--top-k` | `3` | Maximum number of matched skills to return |
| `--min-score` | `1.0` | Minimum score threshold |
| `--include-tag` | _(none)_ | Include only skills containing at least one of these tags (repeatable) |
| `--exclude-tag` | _(none)_ | Exclude skills containing any of these tags (repeatable) |

### `adk-rust deploy` subcommands

These commands are a client for an external deployment control plane: the manifest,
bundling, and API calls live here, and the control plane runs separately. With the
`gcp-deploy` feature, `adk-rust deploy agent-engine` deploys a container image as a Gemini
Enterprise Agent Platform engine.

```bash
adk-rust deploy login --endpoint http://127.0.0.1:8090 --token <bearer-token>
adk-rust deploy logout
adk-rust deploy init                                    # create starter manifest
adk-rust deploy init --agent-name my-agent --binary target/release/my-agent
adk-rust deploy validate --path adk-deploy.toml
adk-rust deploy build --path adk-deploy.toml
adk-rust deploy push --path adk-deploy.toml --env staging
adk-rust deploy status --env production
adk-rust deploy history --env production
adk-rust deploy metrics --env production
adk-rust deploy promote --deployment-id <id>
adk-rust deploy rollback --deployment-id <id>
adk-rust deploy secret set --env production OPENAI_API_KEY sk-...
adk-rust deploy secret list --env production
adk-rust deploy secret delete --env production OPENAI_API_KEY
```

`deploy init` creates a starter `adk-deploy.toml` manifest in the current
directory. Pass `--path` to write to a different location, `--agent-name` to
set the agent name, and `--binary` to set the binary path. Fails if the file
already exists.

Deploy credentials are stored in the OS credential store keyed by control-plane
endpoint. The saved CLI config keeps the endpoint and workspace metadata, but
not the bearer token itself.

### REPL Commands

| Input | Action |
|-------|--------|
| Any text | Send to agent |
| `/help` | Show commands, including those registered with `Launcher::with_console_command` |
| `quit`, `exit`, `/quit`, or `/exit` | Exit |
| `/clear` | Clear display |
| Ctrl+C | Interrupt; at an approval prompt, deny the call and cancel the run |
| Up/Down arrows | History |

### Approvals at the console

The harness asks with `[y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args
[?] details`: `[a]` remembers a standing rule, and a denial ends the turn (see
[The harness in your terminal](../docs/official_docs/harness/cli.md#approvals-at-the-terminal)).
For any other agent, a tool call held for confirmation (`require_tool_confirmation`, or a `ToolPolicy` that
requires approval) is shown and asked about at the terminal:

```text
[tool-call] record_note {"note":"ship it"}

[approval needed] record_note {"note":"ship it"}
  [y] allow once  [n] deny (reason)  [e] edit args  [?] details > y

[approved] record_note

[tool-response] record_note {"recorded":"ship it"}
```

| Answer | Effect |
|--------|--------|
| `y` | Runs the call once, with the edited arguments when `e` changed them |
| `n` | Denies the call; a reason typed at the follow-up prompt is reported to the model |
| `e` | Replaces the arguments with JSON from `$VISUAL`/`$EDITOR`, or typed on one line, then asks again; the policy checks the edited call again |
| `?` | Shows the call id, the acting agent and subject, the tool's origin and declaration digest, and the full arguments |
| Ctrl+C | Denies the call and cancels the run |

Prompts for calls in one batch are asked one after another. When stdin is not a terminal,
every held call is denied with a message, so an unattended run never waits. A prompt waits
up to an hour (`CONSOLE_CONFIRMATION_TIMEOUT`) unless the run configuration sets another
`tool_confirmation_timeout`. The governed path asks through
`ToolConfirmationHandler::decide_approval`; a direct `decide` call, which carries only
approve or deny, offers `[y] allow once  [n] deny  [?] details`.

## Library: Launcher

For custom agents, `Launcher` gives any `Arc<dyn Agent>` a CLI with two modes:

```rust
use std::sync::Arc;

use adk_cli::Launcher;
use adk_core::{Agent, Memory};
use adk_session::SessionService;

async fn launch(
    agent: Arc<dyn Agent>,
    memory: Arc<dyn Memory>,
    sessions: Arc<dyn SessionService>,
) -> adk_core::Result<()> {
    // Parses CLI args: chat by default, `serve --port N` for the web server
    Launcher::new(agent)
        .app_name("my_app")
        .with_memory_service(memory)
        .with_session_service(sessions)
        .run()
        .await
}
```

Or call a mode directly without CLI parsing:

```rust
use std::sync::Arc;

use adk_cli::Launcher;
use adk_core::Agent;

async fn modes(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
    // Console
    Launcher::new(agent.clone()).run_console_directly().await?;

    // Server on 127.0.0.1:8080
    Launcher::new(agent).run_serve_directly(8080).await
}
```

Serve mode listens on `127.0.0.1`; `with_bind_address` listens elsewhere, together with `with_security_config` listing the public host names (`SecurityConfig::allowed_hosts`) and browser origins.

### Builder methods

| Method | Description |
|--------|-------------|
| `new(agent)` | Create a launcher with the given `Arc<dyn Agent>` |
| `app_name(name)` | Custom application name (defaults to agent name) |
| `with_session_service(svc)` | Custom `SessionService` (defaults to `InMemorySessionService`) |
| `with_artifact_service(svc)` | Custom `ArtifactService` for binary artifact storage |
| `with_memory_service(svc)` | Custom `Memory` service for semantic search |
| `with_compaction(config)` | Enable runner-level context compaction in serve mode |
| `with_context_cache(config, model)` | Enable automatic prompt cache lifecycle management |
| `with_security_config(config)` | Custom server security settings: allowed hosts and origins, body size, timeout |
| `with_bind_address(addr)` | Address serve mode listens on (default `127.0.0.1`) |
| `with_request_context_extractor(ext)` | Request context extractor for authenticated deployments |
| `with_a2a_base_url(url)` | Enable A2A routes when building or serving |
| `with_telemetry(config)` | Configure telemetry initialization for serve mode |
| `with_shutdown_grace_period(dur)` | Maximum graceful shutdown window for the web server |
| `with_streaming_mode(mode)` | Set streaming mode (currently affects console mode only); takes precedence over the run configuration's |
| `with_thinking_mode(mode)` | Control how thinking content is rendered in console mode |
| `with_run_config(config)` | `RunConfig` for console runs, merged with the console settings |
| `with_tool_policy(policy)` | `ToolPolicy` for every tool call of a console run |
| `with_confirmation_handler(handler)` | What decides held calls in console runs (default: `ConsoleConfirmationHandler`) |
| `with_user_id(id)` | The user console runs act for (default `"user"`) |
| `with_session_id(id)` | Continue this console session, creating it when it does not exist (default: a new session) |
| `with_console_command(command)` | Register a `ConsoleCommand` slash command |
| `with_approval_service(service)` | Serve the approval inbox at `/api/approvals`, with its expiry ticker |
| `with_background_runs(state)` | Serve background runs at `/api/runs` |

The console-run settings do not reach serve mode, which takes no `RunConfig`; `build_app`
fails while a tool policy is set rather than serve without it. The approval inbox answers
`401` unless `with_request_context_extractor` authenticates the caller.

### Execution methods

| Method | Description |
|--------|-------------|
| `run()` | Parse CLI args and dispatch to chat or serve |
| `run_console_directly()` | Start the REPL without CLI parsing |
| `run_serve_directly(port)` | Start the web server without CLI parsing |
| `build_app()` | Build an `axum::Router` for custom composition, through `ServerBuilder` |
| `build_app_with_a2a(base_url)` | Build an `axum::Router` with A2A routes enabled |

### Production server composition

For production apps that need custom routes, middleware, or ownership of the
serve loop, use `build_app()` instead of `run_serve_directly()`:

```rust
use std::sync::Arc;

use adk_cli::Launcher;
use adk_cli::launcher::TelemetryConfig;
use adk_core::Agent;
use adk_server::SecurityConfig;
use axum::{Router, routing::get};

async fn serve(agent: Arc<dyn Agent>) -> Result<(), Box<dyn std::error::Error>> {
    let app = Launcher::new(agent)
        .with_a2a_base_url("https://agent.example.com")
        .with_security_config(SecurityConfig::default().with_allowed_hosts(["agent.example.com"]))
        .with_telemetry(TelemetryConfig::None)
        .build_app()?;

    let app = app.merge(Router::new().route("/admin/ping", get(|| async { "pong" })));
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

### Telemetry modes

`TelemetryConfig`, `ThinkingDisplayMode`, `StreamPrinter`, and `CONSOLE_CONFIRMATION_TIMEOUT`
live in `adk_cli::launcher`; `ConsoleIo`, `ConsoleConfirmationHandler`, and `ConsoleCommand`
in `adk_cli::console`, re-exported at the crate root.

| Variant | Description |
|---------|-------------|
| `TelemetryConfig::AdkExporter` | Default in-memory ADK exporter with configurable `service_name` |
| `TelemetryConfig::Otlp` | Initialize OTLP export |
| `TelemetryConfig::None` | Skip launcher-managed telemetry initialization |

### Thinking display modes

| Variant | Description |
|---------|-------------|
| `ThinkingDisplayMode::Auto` | Show thinking in a dimmed block, auto-close on next text |
| `ThinkingDisplayMode::Show` | Always show thinking content |
| `ThinkingDisplayMode::Hide` | Suppress all thinking content (both `<think>` tags and `Thinking` parts) |

The `StreamPrinter` type handles rendering of streamed response parts including
text, thinking blocks, function calls/responses, inline data, and file data, a held call as
`[approval needed] <tool> <args> — <reason>`, and an applied decision as `[approved] <tool>`
or `[denied] <tool>`. It prints
terminal control characters other than newline and tab as `\u{..}` escapes, so model or
tool output cannot inject escape sequences into the terminal; stream errors are printed the
same way. `StreamPrinter::with_writer` renders to any `std::io::Write` instead of stdout.

## Configuration Priority

Resolution order (highest wins):

1. CLI flags (`--provider`, `--api-key`, etc.)
2. Environment variables (`GOOGLE_API_KEY`, `OPENAI_API_KEY`, etc.)
3. OS credential store (saved during first-run setup)
4. Saved config (`adk-rust/config.json`) for provider/model only
5. Interactive setup (first run only)

## Provider-Specific Notes

- **Gemini**: Google Search grounding tool is automatically added
- **Anthropic**: `--thinking-budget` enables extended thinking with the given token budget
- **Ollama**: No API key needed; make sure `ollama serve` is running locally
- **Groq**: Free tier available at [console.groq.com](https://console.groq.com)

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-server](https://crates.io/crates/adk-server) — HTTP server
- [adk-runner](https://crates.io/crates/adk-runner) — execution runtime
- [adk-skill](https://crates.io/crates/adk-skill) — skill discovery
- [adk-deploy](https://crates.io/crates/adk-deploy) — deployment manifest and bundling

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `adk-rust serve`, `Launcher` serve mode, and `serve::run_serve` listen on `127.0.0.1` | `serve --host 0.0.0.0` or `Launcher::with_bind_address`, with allowed hosts and origins |
| `adk-rust audit verify`, `audit keygen`, and `audit export` are new | — |
| `StreamPrinter` escapes terminal control characters in model and tool output | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
