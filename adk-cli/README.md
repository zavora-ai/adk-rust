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
adk-rust deploy       Deployment platform commands
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

### `adk-rust deploy` subcommands

```bash
adk-rust deploy login --endpoint http://127.0.0.1:8090 --token <bearer-token>
adk-rust deploy logout
adk-rust deploy validate --path adk-deploy.toml
adk-rust deploy build --path adk-deploy.toml
adk-rust deploy push --path adk-deploy.toml --env staging
adk-rust deploy status --env production
adk-rust deploy history --env production
adk-rust deploy metrics --env production
adk-rust deploy promote --deployment-id <id>
adk-rust deploy rollback --deployment-id <id>
adk-rust deploy secret set --env production OPENAI_API_KEY sk-...
```

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

You can also enable A2A explicitly with `build_app_with_a2a(...)`.

### Serve-mode configuration

`Launcher` now forwards several server/runtime settings that previously required
manual `ServerConfig` wiring:

```rust
Launcher::new(Arc::new(agent))
    .with_compaction(compaction_config)
    .with_context_cache(context_cache_config, cache_capable_model)
    .with_a2a_base_url("https://agent.example.com")
    .with_telemetry(TelemetryConfig::AdkExporter {
        service_name: "my-agent".to_string(),
    });
```

Available telemetry modes:

- `TelemetryConfig::AdkExporter` — default in-memory ADK exporter
- `TelemetryConfig::Otlp` — initialize OTLP export
- `TelemetryConfig::None` — skip launcher-managed telemetry initialization

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

## License

Apache-2.0
