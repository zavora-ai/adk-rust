# cargo-adk

[![Crates.io](https://img.shields.io/crates/v/cargo-adk.svg)](https://crates.io/crates/cargo-adk)
[![Documentation](https://docs.rs/cargo-adk/badge.svg)](https://docs.rs/cargo-adk)
[![License](https://img.shields.io/crates/l/cargo-adk.svg)](../LICENSE)

Scaffolding, build, evaluation, and deployment CLI for [ADK-Rust](https://github.com/zavora-ai/adk-rust) — generate agent projects from composable templates, verify builds, run eval sets against an agent binary, and deploy to a deployment control plane.

## Install

```bash
cargo install cargo-adk
```

`cargo adk bench` and the `adk-bench` dependency are behind the `bench` feature:

```bash
cargo install cargo-adk --features bench
```

## Commands

| Command | Purpose |
|---------|---------|
| `cargo adk new <name>` | Create a project from a template, a pattern, and addons |
| `cargo adk templates` | List templates, patterns, and aliases |
| `cargo adk addons` | List capability addons |
| `cargo adk build` | Compile the project (`cargo build --release`, or `--debug`) |
| `cargo adk validate` | Check a YAML agent definition (`--yaml`) or a Rust source file (`--rust`) |
| `cargo adk deploy` | Build, bundle, and push to a deployment control plane, or deploy to a target: `--target container`, `agent-engine`, `ecs-express`, or `azure-container-apps` |
| `cargo adk eval` | Run eval sets against an agent binary, or gate on drift in an exported activity ledger |
| `cargo adk bench` | Run performance benchmarks against real LLM APIs (feature `bench`) |

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
cargo adk new my-agent --template autonomous # unattended agent on the agent kernel
cargo adk new my-site --template website     # AWP 2.0 website and its edge
cargo adk new my-desk --template vision      # Document desk with the vision pack

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

### `cargo adk deploy --target` — Deploy to a container runtime or a cloud

`--target` deploys `adk-deploy.toml` through a provider-neutral deploy target instead of the
control plane. Each target checks the deployment guarantees first: secrets by reference only, an
explicit session store, and `[auth]` before public ingress.

```bash
# Write the Dockerfile, compose.yaml and kubernetes.yaml to .adk-deploy/container/
cargo adk deploy --target container --plan

# Deploy, then check the endpoint's health and that it refuses unauthenticated calls
cargo adk deploy --target container

# Agent Engine on Google Cloud (cargo install cargo-adk --features gcp)
cargo adk deploy --target agent-engine --image us-central1-docker.pkg.dev/p/agents/a:1.0.0

# Amazon ECS Express Mode (cargo install cargo-adk --features aws)
cargo adk deploy --target ecs-express --image 123456789012.dkr.ecr.us-east-1.amazonaws.com/a:1.0.0

# Azure Container Apps (cargo install cargo-adk --features azure)
cargo adk deploy --target azure-container-apps --image acme.azurecr.io/a:1.0.0
```

| Flag | Description |
|------|-------------|
| `--target` | `platform` (default), `container`, `agent-engine`, `ecs-express`, or `azure-container-apps` |
| `--image` | Image to deploy; default `[targets.<name>] image` |
| `--manifest` | Manifest path (default `adk-deploy.toml`) |
| `--plan` | Write and print what would be created; deploy nothing |
| `--status <id>`, `--destroy <id>` | Report or remove a deployment |
| `--no-verify` | Skip the endpoint check after deploying |

| Feature | Enables |
|---------|---------|
| `gcp` | `--target agent-engine` |
| `aws` | `--target ecs-express` |
| `azure` | `--target azure-container-apps` |

See [Deploy Targets](../docs/official_docs/deployment/deploy-targets.md).

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

Responses are scored with `adk_eval::Evaluator` against `--criteria` (an `EvaluationCriteria` JSON file; default an exact tool trajectory and a response similarity of 0.8). LLM-judged criteria need `--judge-model` and the provider's API key. `--tolerance` (default 0.05) is the largest score drop that is not a regression, and `--turn-timeout` (default 300 seconds) bounds each response. `--num-runs N` runs the eval sets N times and reports each case's pass rate, score mean and standard deviation, and flakiness; `--min-pass-rate` (default 1.0) is the fraction of runs a case must pass, and a saved baseline holds each case's `pass_rate`. `--judge-max-calls` and `--judge-max-cost-usd` bound the judge's calls across every run; the evaluation stops when they are spent. The command exits non-zero when a case fails, the agent misbehaves, the baseline is missing for `--check-regression`, or a score regresses. See the [evaluation guide](../docs/official_docs/evaluation/evaluation.md) for every flag and an example agent binary.

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
| `sequential` | `SequentialAgent`: a researcher, then a writer, sharing one model |
| `parallel` | `ParallelAgent`: two specialists at once, each storing its answer under an output key, then an aggregator that combines them |
| `loop` | `LoopAgent`: a writer and a critic, looping until the critic calls `exit_loop` (at most five rounds) |
| `conditional` | `LlmConditionalAgent`: the model classifies each request and routes it to a `technical` or `general` agent |
| `graph` | Graph workflow with an agent node and per-step checkpoints in memory |
| `realtime` | Gemini Live voice agent: type a message, get a spoken answer and its transcript (`--provider gemini` only) |
| `custom` | Custom agent with a manual `Agent` implementation that sends the user's message to the model |
| `tools` | LLM agent with `#[tool]` custom tools |
| `rag` | RAG agent with vector search over a knowledge base |
| `api` | REST API server exposing the agent over HTTP |
| `agent-engine` | Gemini Enterprise Agent Engine BYOC container: `serve_agent_engine` binary, `Dockerfile`, and `deploy/terraform/` |
| `openai` | OpenAI-powered LLM agent |
| `harness` | The governed harness: `HarnessAgent` over SQLite sessions, approvals, action ledger, standing rules and kernel stores, run by `HarnessLauncher` (`cargo run` is the REPL with approvals, `cargo run -- serve` serves it, `cargo run -- up` runs the agent kernel, `cargo run -- goal` works toward a goal), `.adk/harness.toml`, an `AGENTS.md` skeleton, an example skill, and a scripted smoke test |
| `autonomous` | An agent that runs unattended ([guide](../docs/official_docs/autonomy/index.md)): `HarnessAgent` in `auto` mode on the `autonomy` feature, with every store in `.adk/state.db`, the OS sandbox for `bash`, an approval-gated `publish_status` side effect, a per-run budget, a daily spend cap, a signed audit log and a weekday cron trigger; `cargo run -- up` runs the agent kernel with its home in `.adk`, `cargo run -- goal` works toward a goal on it, and a scripted test checks that publishing waits for a decision |
| `vision` | A Document desk ([getting started](../docs/official_docs/vision/getting-started.md)): `HarnessAgent` in `auto` mode with the vision pack on `vision-local` — PP-OCRv6 reads each page in `inbox/` on the device, a cloud `Describer` over the provider model sees only a field's crop, `.adk/vision.toml` lets it leave as it is and offers each restriction commented — RF-DETR and PP-OCRv6 covering faces and personal text, whole pages denied, crops asked for at the terminal — and tables land in `tables/`; scripted tests check that a describe leaves under the desk's rules, and that under a rule that asks it is held and nothing leaves when the person says no. Takes no addon that serves |
| `website` | An AWP 2.0 website (spec 3.5 §7.17): the site project `adk-website new` scaffolds — `site.toml`, the `paper` theme, a business draft charter with its own site id, capabilities, public terms, draft legal pages, `data/`, the holding page — and an edge binary: `cargo run` serves the release `adk-rust site publish` puts in `data/host` with `search_site` on the AWP edge, `cargo run -- check` builds and runs the launch gate; a smoke test. Takes no addon |
| `managed-agents` | Anthropic Managed Agents session with SSE streaming |

Each template generates `Cargo.toml` with the right dependencies and features, a `src/main.rs` that compiles and runs, `.env.example` with the required API key variables, `README.md`, and `.gitignore`. CI generates a project for every template, pattern and addon and compiles it, failing on any warning in the generated code (`scripts/check-cargo-adk-templates.sh`).

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

`telemetry`, `server` and `docker` work as generated. The others turn on an `adk-rust` feature
and add a starting point to `src/main.rs` — a service or toolset built after the agent, or a
comment — that you wire into the agent yourself.

| Addon | What it adds |
|-------|--------------|
| `telemetry` | ADK telemetry as the subscriber (`telemetry-otlp`): spans exported over OTLP when `OTEL_EXPORTER_OTLP_ENDPOINT` is set, the console log either way |
| `auth` | The `auth` feature (adk-auth) and `AUTH_API_KEY` read from the environment; nothing checks requests until you add it |
| `sessions` | An `InMemorySessionService` to pass to a runner or server |
| `memory` | An `InMemoryMemoryService` to pass to a runner through `MemoryServiceAdapter` |
| `mcp` | The MCP client feature, with the code that connects a server and adds its `McpToolset` in a comment |
| `guardrails` | An input `GuardrailSet` (harmful content, PII redaction) to pass with `.input_guardrails(..)` |
| `eval` | The `eval` feature (adk-eval), for scoring the agent against eval sets |
| `browser` | A `BrowserToolset` over a WebDriver session (`WEBDRIVER_URL`) to pass with `.toolset(..)` |
| `server` | The agent served over A2A by `A2aServer` on `BIND_HOST:PORT` |
| `docker` | Container packaging: `Dockerfile`, `Dockerfile.static`, `.dockerignore` |

`harness`, `autonomous` and `vision` take no `server`, `docker` or `telemetry` addon: their
launcher serves the agent and installs its own span exporter. `website` takes no addon.

The `api` template and the `server` addon listen on `BIND_HOST` (default `127.0.0.1`); the generated Dockerfiles set `BIND_HOST=0.0.0.0`. A server that listens beyond loopback answers only the host names it is configured with (`SecurityConfig::allowed_hosts`).

The `docker` addon emits three build-time files and touches no runtime code:

- `Dockerfile` — multi-stage build: `rust:1.95-slim` build stage (tag kept in lockstep with the workspace `rust-toolchain.toml`, optional `sccache` lines commented out) and a `gcr.io/distroless/cc-debian12` runtime stage with `ENV PORT=8080` and `ENTRYPOINT ["/app/agent"]`.
- `Dockerfile.static` — fully static variant: `x86_64-unknown-linux-musl` build (musl-tools and cmake for `aws-lc-sys`) on a `FROM scratch` runtime that copies in the CA bundle (reqwest's `rustls` verifies against the system trust store, read from `/etc/ssl/certs` at runtime). Works with the `gemini-agent-platform` / `gemini-agent-platform-full` feature sets; incompatible with `livekit` (OpenSSL via native-tls) and the adk-audio `onnx` / `kokoro` / `desktop-audio` features (shared ONNX Runtime, espeak-ng, ALSA).
- `.dockerignore` — excludes `target/`, `.git/`, and `.env` files from the build context.

### Enterprise Patterns (5)

Pre-composed combinations of a base template and addons. Patterns share the `--template` namespace:

| Pattern | Base | Addons | Use case |
|---------|------|--------|----------|
| `multi-agent` | sequential | telemetry | Two agents in sequence with tracing |
| `production` | llm | server, auth, sessions, telemetry | An agent served over A2A with tracing, and the auth and sessions starting points |
| `pipeline` | sequential | sessions, telemetry | Sequential agents with tracing and a session service |
| `chatbot` | llm | sessions, memory, server | An agent served over A2A, with session and memory services to wire in |
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
| `cargo adk bench --suite` is removed; it ran the ordinary workloads whatever suite it named | Drop the flag |
| `cargo adk bench` is behind the `bench` feature, off by default; without it the subcommand exits 2 with the install command | `cargo install cargo-adk --features bench` |
| New templates: `harness`, `autonomous`, `website` and `vision` | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## License

Apache-2.0

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
