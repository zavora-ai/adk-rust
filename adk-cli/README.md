# adk-cli

Command-line launcher for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-cli.svg)](https://crates.io/crates/adk-cli)
[![Documentation](https://docs.rs/adk-cli/badge.svg)](https://docs.rs/adk-cli)
[![License](https://img.shields.io/crates/l/adk-cli.svg)](../LICENSE)

## Overview

`adk-cli` provides two things:

- **`adk-rust` binary** — chat with an AI agent (6 providers), serve a web UI, run the
  **coding agent** (`code` / `goal` / `ultracode`), manage skills, inspect graph checkpoints,
  verify signed audit logs, and deploy
- **`Launcher` library** — embed a REPL and web server into any custom agent binary

## Quick Start

```bash
cargo install adk-cli

# Just run it — interactive setup picks your provider on first run
adk-rust

# Or pre-configure a provider
adk-rust --provider openai --api-key sk-...

# Equivalent to:
adk-rust chat

# Web server
adk-rust serve --port 3000

# Coding agent (sandboxed to the directory)
adk-rust code "make the failing test pass"
adk-rust goal "all tests green" --until "cargo test" --resume   # autonomous, durable
adk-rust ultracode "add input validation"                       # parallel ultra-review
```

See the [Coding Agent guide](../docs/official_docs/coding-agent/cli.md) for `code` / `goal` / `ultracode`.

## Feature Flags

| Feature | Default | Enables |
|---------|---------|---------|
| `gemini` | yes | The Gemini provider |
| `audit` | yes | `adk-rust audit verify` and `audit keygen` (through `adk-auth`'s `signed-audit`) |
| `openai`, `anthropic`, `deepseek`, `groq`, `ollama` | no | The other providers |
| `all-providers` | no | Every provider |
| `audit-sql` | no | `adk-rust audit export` from SQLite or PostgreSQL audit stores |
| `graph-sqlite` | no | The SQLite checkpointer for `adk-rust graph` |
| `gcp-deploy` | no | `adk-rust deploy agent-engine` |
| `vertex-agent-registry` | no | `adk-rust registry` |
| `vertex-skill-registry` | no | Skill Registry lookups in `adk-rust skills` |
| `telemetry-otlp` | no | OTLP export for `TelemetryConfig::Otlp` |
| `optimize` | no | The `Launcher` CLI's `optimize` subcommand: instruction optimization against an evaluation set |

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

If no provider is configured, `adk-rust` launches an interactive setup:

1. Choose a provider from the menu
2. Enter your API key (skipped for Ollama)
3. Provider and model are saved to `adk-rust/config.json` in the platform config directory (`~/.config` on Linux, `~/Library/Application Support` on macOS, `%APPDATA%` on Windows)
4. API keys are stored in your OS credential store (Keychain, Credential Manager, Secret Service)

On subsequent runs, the saved config is used automatically. CLI flags always
take priority over environment variables, secure credential storage, and saved config.

## Binary Commands

```text
adk-rust              Interactive REPL (default, same as `chat`)
adk-rust chat         Interactive REPL with an AI agent
adk-rust serve        Start web server with an AI agent
adk-rust code         Run the coding agent on a task in a workspace directory
adk-rust goal         Work until a success-condition command exits 0
adk-rust ultracode    Implement, then review with parallel specialist reviewers
adk-rust skills       Skill tooling (list/validate/match)
adk-rust graph        Graph checkpoint inspection (steps/replay/fork/resume)
adk-rust audit        Signed audit log tools (verify/keygen; export with `audit-sql`)
adk-rust deploy       Deployment commands
adk-rust registry     Agent Registry registration and search (`vertex-agent-registry`)
```

### Global options (apply to `chat` and `serve`)

| Flag | Default | Description |
|------|---------|-------------|
| `--provider` | saved config or interactive | LLM provider |
| `--model` | provider default | Model name (provider-specific) |
| `--api-key` | secure store / env var | API key (overrides all other sources) |
| `--instruction` | built-in default | Agent system prompt |
| `--thinking-budget` | none | Enable provider-side thinking when supported |
| `--thinking-mode` | `auto` | Render emitted thinking: `auto`, `show`, `hide` |

### `adk-rust serve` options

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `8080` | Server port |
| `--host` | `127.0.0.1` | Listen address; `0.0.0.0` listens on every interface |

The server answers only loopback names and IP addresses, and allows no cross-origin browser
access, unless `SecurityConfig` lists more hosts and origins.

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
| `/help` | Show commands |
| `quit`, `exit`, `/quit`, or `/exit` | Exit |
| `/clear` | Clear display |
| Ctrl+C | Interrupt |
| Up/Down arrows | History |

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
| `with_streaming_mode(mode)` | Set streaming mode (currently affects console mode only) |
| `with_thinking_mode(mode)` | Control how thinking content is rendered in console mode |

### Execution methods

| Method | Description |
|--------|-------------|
| `run()` | Parse CLI args and dispatch to chat or serve |
| `run_console_directly()` | Start the REPL without CLI parsing |
| `run_serve_directly(port)` | Start the web server without CLI parsing |
| `build_app()` | Build an `axum::Router` for custom composition |
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

`TelemetryConfig`, `ThinkingDisplayMode`, and `StreamPrinter` live in `adk_cli::launcher`.

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
text, thinking blocks, function calls/responses, inline data, and file data. It prints
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
