# cargo-adk

[![Crates.io](https://img.shields.io/crates/v/cargo-adk.svg)](https://crates.io/crates/cargo-adk)
[![Documentation](https://docs.rs/cargo-adk/badge.svg)](https://docs.rs/cargo-adk)
[![License](https://img.shields.io/crates/l/cargo-adk.svg)](../LICENSE)

Scaffolding, build, evaluation, and deployment CLI for [ADK-Rust](https://github.com/zavora-ai/adk-rust) — generate agent projects from composable templates, verify builds, run eval sets against an agent binary, and deploy to a deployment control plane.

## Install

```bash
cargo install cargo-adk
```

## Commands

| Command | Purpose |
|---------|---------|
| `cargo adk new <name>` | Create a project from a template, a pattern, and addons |
| `cargo adk templates` | List templates, patterns, and aliases |
| `cargo adk addons` | List capability addons |
| `cargo adk build` | Compile the project (`cargo build --release`, or `--debug`) |
| `cargo adk validate` | Check a YAML agent definition (`--yaml`) or a Rust source file (`--rust`) |
| `cargo adk deploy` | Build, bundle, and push to a deployment control plane |
| `cargo adk eval` | Run eval sets against an agent binary, or gate on drift in an exported activity ledger |
| `cargo adk bench` | Run performance benchmarks against real LLM APIs |

### `cargo adk new` — Scaffold a new agent

```bash
# A Gemini LLM agent with an interactive console (template `basic`, an alias for `llm`)
cargo adk new my-agent

# A specific template
cargo adk new my-agent --template tools      # custom tools with the #[tool] macro
cargo adk new my-agent --template rag        # RAG with vector search
cargo adk new my-agent --template api        # REST API server
cargo adk new my-agent --template graph      # graph-based workflow
cargo adk new my-agent --template realtime   # realtime voice/audio agent
cargo adk new my-agent --template harness    # governed harness: REPL with approvals

# Compose with addons
cargo adk new my-agent --template tools --addon telemetry --addon auth

# An enterprise pattern (patterns share the --template namespace)
cargo adk new my-service --template production

# Another provider or model
cargo adk new my-agent --provider anthropic
cargo adk new my-agent --provider openai --model gpt-5.6-terra
```

| Flag | Description |
|------|-------------|
| `-t, --template` | Template, pattern, or alias (default `basic`) |
| `-p, --provider` | LLM provider (default: the template's provider, `gemini` for most) |
| `-m, --model` | Model ID, overriding the provider default |
| `--addon` | Capability addon; repeatable |
| `--output-dir` | Create the project at `<output-dir>/<name>/` |
| `--template-dir` | Directory of custom template manifests (`*.toml`) |
| `--with-yaml` | Also generate a YAML agent definition |
| `--non-interactive` | Never prompt; use defaults or fail |
| `--json-output` | Emit structured JSON instead of text |
| `--dry-run` | Preview the files without writing them |

### `cargo adk build` — Compile without deploying

```bash
# Build in release mode (default)
cargo adk build

# Build in debug mode for faster iteration
cargo adk build --debug

# Build a project at a specific path
cargo adk build --manifest-path /path/to/my-agent/Cargo.toml
```

| Aspect | `cargo adk build` | `cargo adk deploy` |
|--------|-------------------|-------------------|
| **Purpose** | Compile and verify | Compile, bundle, and push to the control plane |
| **Network required** | No | Yes |
| **Authentication** | None | Token required |
| **Output** | Local binary in `target/` | Bundle uploaded to the control plane |

### `cargo adk deploy` — Deploy to a control plane

```bash
# Deploy to a local control plane (default server)
cargo adk deploy

# Deploy to a specific environment and server
cargo adk deploy --environment staging --server https://platform.example.com

# Use a specific auth token
cargo adk deploy --token my-deploy-token

# Skip the build (use the existing binary)
cargo adk deploy --skip-build

# Validate without pushing (CI-friendly)
cargo adk deploy --dry-run
```

| Flag | Default | Description |
|------|---------|-------------|
| `--environment` | `production` | Target deployment environment |
| `--token` | `ADK_DEPLOY_TOKEN` env | Auth token for the control plane |
| `--server` | `http://127.0.0.1:8090` | Control plane URL |
| `--skip-build` | `false` | Skip `cargo build --release` |
| `--dry-run` | `false` | Validate everything without pushing |
| `--workspace-id` | — | Scope the deployment to a workspace |
| `--agent-id` | — | Link the deployment to an existing agent record |
| `--stream-output` | `false` | Emit progress as newline-delimited JSON events |

The deploy command authenticates in this order:

1. `--token` flag (highest priority)
2. `ADK_DEPLOY_TOKEN` environment variable
3. Cached credentials for the same server from `adk-deploy/config.json` in the platform config directory
4. Ephemeral login as `ADK_DEPLOY_EMAIL` (default `cli@local`)

If `adk-deploy.toml` declares secrets and a `.env` file exists, the CLI uploads the matching secrets before pushing:

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

`UPPER_SNAKE_CASE` env var names map to `lower-kebab-case` secret keys: `GOOGLE_API_KEY` → `google-api-key`, `DATABASE_URL` → `database-url`.

Deploy flow:

1. Load and validate `adk-deploy.toml`
2. Authenticate with the control plane
3. Build the release binary (unless `--skip-build`)
4. Upload secrets from `.env` (if present)
5. Create a `.tar.gz` bundle (manifest and binary)
6. Compute the SHA-256 checksum
7. Push to the control plane

### `cargo adk eval` — Run eval sets against an agent binary

```bash
cargo adk eval tests/ --agent-cmd "target/release/eval_agent"
cargo adk eval tests/weather.test.json --agent-cmd "python3 agent.py" --save-baseline
cargo adk eval tests/ --agent-cmd "target/release/eval_agent" --check-regression --format junit --output results.xml
```

The agent runs as its own process: it reads one JSON request per line on stdin and writes one JSON response per line on stdout.

| Line | Shape |
|------|-------|
| Request | `{"case_id", "turn", "user_text", "session_id"}` — every turn of a case shares `session_id` |
| Response | `{"text", "tool_calls": [{"name", "args"}]}` |
| Error response | `{"error"}` — fails the case |

Responses are scored with `adk_eval::Evaluator` against `--criteria` (an `EvaluationCriteria` JSON file; default an exact tool trajectory and a response similarity of 0.8). LLM-judged criteria need `--judge-model` and the provider's API key. `--tolerance` (default 0.05) is the largest score drop that is not a regression, and `--turn-timeout` (default 300 seconds) bounds each response. The command exits non-zero when a case fails, the agent misbehaves, the baseline is missing for `--check-regression`, or a score regresses. See the [evaluation guide](../docs/official_docs/evaluation/evaluation.md) for every flag and an example agent binary.

#### `cargo adk eval --ledger` — Gate on drift in an activity ledger

```bash
cargo adk eval --ledger ledger.jsonl --floor tool_call.duration_ms=50
cargo adk eval --ledger ledger.jsonl --format json --output ledger-report.json
```

`--ledger` takes an exported activity ledger, one camelCase JSON row per line (`ts`, `partition`, `agent`, `kind`, and optional `effect`, `durationMs`, `subjectHash`, `meta`, `traceId`), in place of the eval-set path and `--agent-cmd`. It derives daily dimensions — first and last activity, rows and median duration per kind, numeric `meta` sums — and tests the export's last day with `adk_eval::ledger`: the last 7 and 14 days against the median and MAD of the 28 days before them, per weekday and weekend, `|recent − median| > max(2.5 × MAD, floor)`. The command exits non-zero on a meaningful deviation (two dimensions, or one for 10 days) and on an export too short to finish the 14-day warm-up. `--floor DIMENSION=VALUE` sets a dimension's floor; the defaults are 45 minutes for the two time-of-day dimensions and 0 for the rest.

## Templates

| Template | What you get |
|----------|--------------|
| `llm` (alias `basic`, the default) | Single LLM agent with tool calling and an interactive console |
| `sequential` | Sequential multi-agent pipeline |
| `parallel` | Parallel multi-agent execution with result aggregation |
| `loop` | Loop agent that iterates until a condition is met |
| `conditional` | Conditional agent that routes on an LLM decision |
| `graph` | Graph workflow with checkpoints and durable execution |
| `realtime` | Realtime bidirectional audio/video streaming agent |
| `custom` | Custom agent with a manual trait implementation |
| `tools` | LLM agent with `#[tool]` custom tools |
| `rag` | RAG agent with vector search over a knowledge base |
| `api` | REST API server exposing the agent over HTTP |
| `agent-engine` | Gemini Enterprise Agent Engine BYOC container: `serve_agent_engine` binary, `Dockerfile`, and `deploy/terraform/` |
| `openai` | OpenAI-powered LLM agent |
| `harness` | The governed harness: `HarnessAgent` over SQLite sessions run by `HarnessLauncher` (`cargo run` is the REPL with approvals, `cargo run -- serve` serves it), `.adk/harness.toml`, an `AGENTS.md` skeleton, an example skill, and a scripted smoke test |
| `managed-agents` | Anthropic Managed Agents session with SSE streaming |

Each template generates `Cargo.toml` with the right dependencies and features, a `src/main.rs` that compiles and runs, `.env.example` with the required API key variables, `README.md`, and `.gitignore`.

## Composable Template System

The `--addon` flag layers cross-cutting capabilities onto any base template:

```bash
# Add telemetry and auth to a tools agent
cargo adk new my-agent --template tools --addon telemetry --addon auth

# Add container packaging to an API server
cargo adk new my-agent --template api --addon docker

# Combine several addons
cargo adk new my-agent --template llm --addon server --addon telemetry --addon docker
```

### Addons (10)

| Addon | What it adds |
|-------|--------------|
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

The `api` template and the `server` addon listen on `BIND_HOST` (default `127.0.0.1`); the generated Dockerfiles set `BIND_HOST=0.0.0.0`. A server that listens beyond loopback answers only the host names it is configured with (`SecurityConfig::allowed_hosts`).

The `docker` addon emits three build-time files and touches no runtime code:

- `Dockerfile` — multi-stage build: `rust:1.95-slim` build stage (tag kept in lockstep with the workspace `rust-toolchain.toml`, optional `sccache` lines commented out) and a `gcr.io/distroless/cc-debian12` runtime stage with `ENV PORT=8080` and `ENTRYPOINT ["/app/agent"]`.
- `Dockerfile.static` — fully static variant: `x86_64-unknown-linux-musl` build (musl-tools and cmake for `aws-lc-sys`) on a `FROM scratch` runtime that copies in the CA bundle (`rustls-tls-native-roots` reads `/etc/ssl/certs` at runtime). Works with the `gemini-agent-platform` / `gemini-agent-platform-full` feature sets; incompatible with `livekit` (OpenSSL via native-tls) and the adk-audio `onnx` / `kokoro` / `desktop-audio` features (shared ONNX Runtime, espeak-ng, ALSA).
- `.dockerignore` — excludes `target/`, `.git/`, and `.env` files from the build context.

### Enterprise Patterns (5)

Pre-composed combinations of a base template and addons. Patterns share the `--template` namespace:

| Pattern | Base | Addons | Use case |
|---------|------|--------|----------|
| `multi-agent` | sequential | telemetry | Multi-agent supervisor with observability |
| `production` | llm | server, auth, sessions, telemetry | Production-ready agent service |
| `pipeline` | sequential | sessions, telemetry | Sequential data processing pipeline |
| `chatbot` | llm | sessions, memory, server | Conversational chatbot with memory and HTTP interface |
| `a2a-server` (alias `a2a`) | llm | server, sessions | A2A protocol server with session management |

```bash
# Use an enterprise pattern
cargo adk new my-service --template production

# Extend a pattern with additional addons
cargo adk new my-service --template production --addon docker
```

For full documentation on templates, addons, and patterns, see the [Composable Templates Guide](../docs/official_docs/development/composable-templates.md).

## Generated Project

```text
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
```

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `cargo adk eval` runs an agent binary over a line protocol; `--agent-cmd` is required, and the command fails closed | Add `--agent-cmd`; the ignored `--model` and `--concurrency` flags are removed |
| An eval set without cases is an error | — |
| The `api` template and the `server` addon bind `BIND_HOST` (default `127.0.0.1`) | Set `BIND_HOST=0.0.0.0` where the server must be reachable from other machines |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## License

Apache-2.0

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
