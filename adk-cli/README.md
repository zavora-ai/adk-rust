# adk-cli

Command-line launcher for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-cli.svg)](https://crates.io/crates/adk-cli)
[![Documentation](https://docs.rs/adk-cli/badge.svg)](https://docs.rs/adk-cli)
[![License](https://img.shields.io/crates/l/adk-cli.svg)](LICENSE)

## Overview

`adk-cli` provides two things:

- **`adk-rust` binary** — chat with an AI agent (6 providers), serve a web UI, or manage skills
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
```

## Supported Providers

| Provider | Flag | Default Model | Env Var |
|----------|------|---------------|---------|
| Gemini | `--provider gemini` | `gemini-2.5-flash` | `GOOGLE_API_KEY` / `GEMINI_API_KEY` |
| OpenAI | `--provider openai` | `gpt-4.1` | `OPENAI_API_KEY` |
| Anthropic | `--provider anthropic` | `claude-sonnet-4-5-20250929` | `ANTHROPIC_API_KEY` |
| DeepSeek | `--provider deepseek` | `deepseek-chat` | `DEEPSEEK_API_KEY` |
| Groq | `--provider groq` | `llama-3.3-70b-versatile` | `GROQ_API_KEY` |
| Ollama | `--provider ollama` | `llama3.2` | _(none, local)_ |

## First-Run Setup

If no provider is configured, `adk-rust` launches an interactive setup:

1. Choose a provider from the menu
2. Enter your API key (skipped for Ollama)
3. Provider and model are saved to `~/.config/adk-rust/config.json`
4. API keys are stored in your OS credential store (Keychain, Credential Manager, Secret Service)

On subsequent runs, the saved config is used automatically. CLI flags always
take priority over environment variables, secure credential storage, and saved config.

## Binary Commands

```
adk-rust              Interactive REPL (default, same as `chat`)
adk-rust chat         Interactive REPL with an AI agent
adk-rust serve        Start web server with an AI agent
adk-rust skills       Skill tooling (list/validate/match)
adk-rust deploy       External platform integration commands
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

These commands target the external `adk-platform` deployment product. In
`adk-rust`, the deploy surface is kept as a client and manifest/bundling
integration layer; the control plane and operator console live in the separate
`adk-platform` repository.

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
use adk_cli::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let agent = create_your_agent()?;

    // Parses CLI args: default is chat, `serve --port N` for web
    Launcher::new(Arc::new(agent))
        .app_name("my_app")
        .with_memory_service(my_memory)
        .with_session_service(my_sessions)
        .run()
        .await
}
```

Or call modes directly without CLI parsing:

```rust
// Console directly
Launcher::new(Arc::new(agent))
    .run_console_directly()
    .await?;

// Server directly
Launcher::new(Arc::new(agent))
    .run_serve_directly(8080)
    .await?;
```

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
| `with_security_config(config)` | Custom server security settings |
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
use adk_cli::{Launcher, TelemetryConfig};
use std::sync::Arc;

let app = Launcher::new(Arc::new(agent))
    .with_a2a_base_url("https://agent.example.com")
    .with_telemetry(TelemetryConfig::None)
    .build_app()?;

let app = app.merge(my_admin_routes());
let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

### Telemetry modes

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
text, thinking blocks, function calls/responses, inline data, and file data.

## Configuration Priority

Resolution order (highest wins):

1. CLI flags (`--provider`, `--api-key`, etc.)
2. Environment variables (`GOOGLE_API_KEY`, `OPENAI_API_KEY`, etc.)
3. OS credential store (saved during first-run setup)
4. Saved config (`~/.config/adk-rust/config.json`) for provider/model only
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

## License

Apache-2.0
