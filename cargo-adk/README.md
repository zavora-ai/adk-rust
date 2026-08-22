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
| `agent-engine` | Gemini Enterprise Agent Engine BYOC container: `serve_agent_engine` binary, `Dockerfile`, and `deploy/terraform/` |

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

# Add container packaging to an API server
cargo adk new my-agent --template api --addon docker

# Combine multiple addons
cargo adk new my-agent --template llm --addon server --addon telemetry --addon docker
```

### Available Addons (10)

| Addon | What it adds |
|-------|-------------|
| `telemetry` | OpenTelemetry tracing integration |
| `auth` | API key and JWT authentication |
| `sessions` | Session state management and persistence |
| `memory` | Semantic memory and RAG search integration |
| `mcp` | Model Context Protocol server connections |
| `guardrails` | Input/output validation and content filtering |
| `eval` | Evaluation framework for agent quality testing |
| `browser` | Browser automation tools via WebDriver |
| `server` | HTTP server with A2A protocol support |
| `docker` | Container packaging: `Dockerfile`, `Dockerfile.static`, `.dockerignore` |

The `docker` addon emits three build-time files and touches no runtime code:

- `Dockerfile` — multi-stage build: `rust:1.95-slim` build stage (tag kept in lockstep with the workspace `rust-toolchain.toml`, optional `sccache` lines commented out) and a `gcr.io/distroless/cc-debian12` runtime stage with `ENV PORT=8080` and `ENTRYPOINT ["/app/agent"]`.
- `Dockerfile.static` — fully static variant: `x86_64-unknown-linux-musl` build (musl-tools + cmake for `aws-lc-sys`) on a `FROM scratch` runtime that copies in the CA bundle (`rustls-tls-native-roots` reads `/etc/ssl/certs` at runtime). Works with the `gemini-agent-platform` / `gemini-agent-platform-full` feature sets; incompatible with `livekit` (OpenSSL via native-tls) and the adk-audio `onnx` / `kokoro` / `desktop-audio` features (shared ONNX Runtime, espeak-ng, ALSA).
- `.dockerignore` — excludes `target/`, `.git/`, and `.env` files from the build context.

### Enterprise Patterns (5)

Pre-composed combinations of a base template and curated addons for production scenarios. Patterns share the `--template` namespace:

| Pattern | Base | Addons | Use case |
|---------|------|--------|----------|
| `multi-agent` | sequential | telemetry | Multi-agent supervisor with observability |
| `production` | llm | server, auth, sessions, telemetry | Production-ready agent service |
| `pipeline` | sequential | sessions, telemetry | Sequential data processing pipeline |
| `chatbot` | llm | sessions, memory, server | Conversational chatbot with memory and HTTP interface |
| `a2a-server` | llm | server, sessions | A2A protocol server with session management |

```bash
# Use an enterprise pattern
cargo adk new my-service --template production

# Extend a pattern with additional addons
cargo adk new my-service --template production --addon docker
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
cargo-adk = "2.1.0"
```

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.

## License

Apache-2.0
