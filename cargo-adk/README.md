# cargo-adk

[![crates.io](https://img.shields.io/crates/v/cargo-adk.svg)](https://crates.io/crates/cargo-adk)

Scaffolding, build, and deployment CLI for [ADK-Rust](https://github.com/zavora-ai/adk-rust) — generate agent projects from composable templates, verify builds, and deploy to ADK Platform.

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
cargo adk new my-agent --template tools      # custom tools with #[tool] macro
cargo adk new my-agent --template rag        # RAG with vector search
cargo adk new my-agent --template api        # REST API server
cargo adk new my-agent --template openai     # OpenAI-powered agent
cargo adk new my-agent --template a2a        # A2A protocol server
cargo adk new my-agent --template graph      # Graph-based workflow
cargo adk new my-agent --template realtime   # Real-time voice/audio agent

# Compose with addons
cargo adk new my-agent --template tools --addon telemetry --addon auth

# Use an enterprise pattern
cargo adk new my-agent --pattern microservices

# Use a different provider
cargo adk new my-agent --provider anthropic
cargo adk new my-agent --provider openai

# List available templates
cargo adk templates
```

### `cargo adk build` — Compile without deploying

Verify that your agent project compiles correctly without deploying. Useful for local development and CI pipelines.

```bash
# Build in release mode (default)
cargo adk build

# Build in debug mode for faster iteration
cargo adk build --debug

# Build a project at a specific path
cargo adk build --manifest-path /path/to/my-agent/Cargo.toml
```

#### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--manifest-path <PATH>` | Current directory | Path to the `Cargo.toml` file |
| `--debug` | Release mode | Build in debug mode (faster compilation, unoptimized binary) |

#### Build vs Deploy

| Aspect | `cargo adk build` | `cargo adk deploy` |
|--------|-------------------|-------------------|
| **Purpose** | Compile and verify | Compile, bundle, and push to platform |
| **Network required** | No | Yes |
| **Authentication** | None | Token required |
| **Output** | Local binary in `target/` | Bundle uploaded to platform |
| **Use case** | Local dev, CI checks | Production deployment |

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
| `openai` | OpenAI GPT-4o agent with console |
| `a2a` | A2A protocol server with `A2aServer::quick_start` |
| `graph` | Graph-based workflow with checkpoints and durable execution |
| `realtime` | Real-time bidirectional audio/video streaming agent |

Each template generates:

- `Cargo.toml` with the right dependencies and feature flags
- `src/main.rs` that compiles and runs immediately
- `.env.example` with the required API key variables
- `README.md` with setup instructions

## Composable Template System

The `--addon` flag lets you layer cross-cutting capabilities onto any base template:

```bash
# Add telemetry and auth to a tools agent
cargo adk new my-agent --template tools --addon telemetry --addon auth

# Add Docker and CI to an API server
cargo adk new my-agent --template api --addon docker --addon ci

# Combine multiple addons
cargo adk new my-agent --template a2a --addon telemetry --addon monitoring --addon docker
```

### Available Addons (9)

| Addon | What it adds |
|-------|-------------|
| `telemetry` | OpenTelemetry integration with OTLP exporter |
| `auth` | Authentication middleware (API keys, JWT, OAuth2) |
| `eval` | Evaluation framework with trajectory and semantic scoring |
| `docker` | Multi-stage Dockerfile and docker-compose.yml |
| `ci` | GitHub Actions CI pipeline (fmt, clippy, test, build) |
| `monitoring` | Health checks, readiness probes, Prometheus metrics |
| `tracing` | Structured logging with configurable levels |
| `logging` | File-based logging with rotation |
| `testing` | Property-based testing and integration test scaffolding |

### Enterprise Patterns (5)

Pre-composed combinations of a base template and curated addons for production scenarios:

| Pattern | Base | Addons | Use case |
|---------|------|--------|----------|
| `microservices` | api | telemetry, monitoring, docker, ci | Kubernetes-ready agent microservices |
| `event-driven` | graph | telemetry, monitoring, logging | Event-driven workflows with durable execution |
| `multi-agent` | basic | telemetry, tracing, monitoring | Multi-agent orchestration with observability |
| `serverless` | basic | telemetry, logging | AWS Lambda / Cloud Functions deployment |
| `data-pipeline` | basic | telemetry, eval, logging, testing | ETL and document processing pipelines |

```bash
# Use an enterprise pattern
cargo adk new my-service --pattern microservices

# Extend a pattern with additional addons
cargo adk new my-service --pattern microservices --addon auth
```

For full documentation on all templates, addons, and patterns, see the [Composable Templates Guide](../docs/official_docs/development/composable-templates.md).

## Generated Project

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
├── .env.example
├── README.md
└── .gitignore
```

```bash
cd my-agent
cp .env.example .env    # add your API key
cargo run               # interactive console
cargo adk build         # verify compilation
cargo adk deploy        # push to platform
```

## Version

Current version: **1.0.0**

```toml
[dependencies]
cargo-adk = "1.0.1"
```

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.

## License

Apache-2.0
