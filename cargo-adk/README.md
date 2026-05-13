# cargo-adk

[![crates.io](https://img.shields.io/crates/v/cargo-adk.svg)](https://crates.io/crates/cargo-adk)

Scaffolding and deployment CLI for [ADK-Rust](https://github.com/zavora-ai/adk-rust) — generate agent projects from templates and deploy them to ADK Platform.

## Install

```bash
cargo install cargo-adk
```

## Commands

### `cargo adk new` — Scaffold a new agent

```bash
# Create a basic Gemini agent
cargo adk new my-agent

# Create with a specific template
cargo adk new my-agent --template tools    # custom tools with #[tool] macro
cargo adk new my-agent --template rag      # RAG with vector search
cargo adk new my-agent --template api      # REST API server
cargo adk new my-agent --template openai   # OpenAI-powered agent

# Use a different provider
cargo adk new my-agent --provider anthropic
cargo adk new my-agent --provider openai

# List available templates
cargo adk templates
```

### `cargo adk deploy` — Deploy to ADK Platform

```bash
# Deploy to local platform (default)
cargo adk deploy

# Deploy to a specific environment and server
cargo adk deploy --environment staging --server https://platform.example.com

# Use a specific auth token
cargo adk deploy --token my-deploy-token

# Skip build (use existing binary)
cargo adk deploy --skip-build

# Validate without pushing (CI-friendly)
cargo adk deploy --dry-run
```

#### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--environment` | `production` | Target deployment environment |
| `--token` | `ADK_DEPLOY_TOKEN` env | Auth token for the platform server |
| `--server` | `http://127.0.0.1:8090` | Platform server URL |
| `--skip-build` | `false` | Skip `cargo build --release` |
| `--dry-run` | `false` | Validate everything without pushing |

#### Authentication

The deploy command authenticates in this order:

1. `--token` flag (highest priority)
2. `ADK_DEPLOY_TOKEN` environment variable
3. Cached credentials from `~/.config/adk-deploy/config.json`
4. Ephemeral login (requires `ADK_DEPLOY_EMAIL` env var)

#### Secret Upload

If your `adk-deploy.toml` declares secrets and a `.env` file exists, the CLI automatically uploads matching secrets before pushing:

```toml
# adk-deploy.toml
[[secrets]]
key = "google-api-key"
required = true
```

```bash
# .env
GOOGLE_API_KEY=your-actual-key
```

The convention maps `UPPER_SNAKE_CASE` env var names to `lower-kebab-case` secret keys:
- `GOOGLE_API_KEY` → `google-api-key`
- `OPENAI_API_KEY` → `openai-api-key`
- `DATABASE_URL` → `database-url`

#### Deploy Flow

1. Load and validate `adk-deploy.toml`
2. Authenticate with the platform
3. Upload secrets from `.env` (if present)
4. Build the release binary
5. Create a `.tar.gz` bundle (manifest + binary)
6. Compute SHA-256 checksum
7. Push to the platform server

## Templates

| Template | What you get |
|----------|-------------|
| `basic` | Gemini agent with interactive console (default) |
| `tools` | Agent with `#[tool]` macro custom tools |
| `rag` | RAG pipeline with Gemini embeddings + in-memory vector store |
| `api` | REST server with health check, ready for deployment |
| `openai` | OpenAI GPT-5-mini agent with console |

Each template generates:

- `Cargo.toml` with the right dependencies and feature flags
- `src/main.rs` that compiles and runs immediately
- `.env.example` with the required API key variables
- `.gitignore`

## Generated Project

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
├── .env.example
└── .gitignore
```

```bash
cd my-agent
cp .env.example .env    # add your API key
cargo run               # interactive console
cargo adk deploy        # push to platform
```

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.

## License

Apache-2.0
