# Composable Template System

## Overview

The ADK-Rust composable template system provides a flexible scaffolding engine for creating new agent projects. Instead of a single monolithic project generator, the system separates concerns into three layers:

- **Base Templates** define the core agent architecture (8 templates)
- **Addons** inject cross-cutting capabilities like telemetry, auth, and testing (9 addons)
- **Enterprise Patterns** combine a base template with curated addons for production scenarios (5 patterns)

This composable approach lets you start with exactly what you need and layer on capabilities as your project grows. Create a minimal agent in seconds, then add telemetry, authentication, or CI configuration without restructuring your project.

All scaffolding is done through the `cargo adk new` command:

```bash
# Basic usage
cargo adk new my-agent --template basic

# With addons
cargo adk new my-agent --template tools --addon telemetry --addon auth

# Enterprise pattern (pre-composed template + addons)
cargo adk new my-agent --pattern microservices
```

## Base Templates

ADK-Rust ships with 8 base templates covering the most common agent architectures. Each template generates a complete, runnable project with the appropriate dependencies and boilerplate.

### basic

A minimal single-agent project with LLM integration. The simplest starting point for any agent.

**Command:**

```bash
cargo adk new my-agent --template basic
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
├── .env.example
└── README.md
```

**Description:** Generates a single `LlmAgent` with Gemini as the default provider. Includes basic tracing setup, environment variable loading via `dotenvy`, and a simple runner invocation. Ideal for prototyping and learning the ADK-Rust API.

---

### tools

An agent with function tool calling support. Extends the basic template with tool definitions and registration.

**Command:**

```bash
cargo adk new my-agent --template tools
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   └── tools.rs
├── .env.example
└── README.md
```

**Description:** Generates an `LlmAgent` with one or more example tools registered via the `#[tool]` macro. The `tools.rs` module demonstrates how to define tools with typed parameters, return values, and documentation. Shows the complete tool-calling lifecycle from definition to agent integration.

---

### rag

A retrieval-augmented generation agent with semantic memory integration.

**Command:**

```bash
cargo adk new my-agent --template rag
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── tools.rs
│   └── memory.rs
├── data/
│   └── .gitkeep
├── .env.example
└── README.md
```

**Description:** Generates an agent configured with a memory service for semantic search over documents. The `memory.rs` module sets up an in-memory vector store (swappable to PostgreSQL/pgvector in production). Includes a retrieval tool that searches the memory store and injects relevant context into the agent's prompt.

---

### api

An agent exposed as an HTTP API server with the A2A (Agent-to-Agent) protocol.

**Command:**

```bash
cargo adk new my-agent --template api
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   └── agent.rs
├── .env.example
└── README.md
```

**Description:** Generates an agent wrapped in an Axum HTTP server with A2A protocol endpoints. The server exposes `/tasks/send`, `/tasks/sendSubscribe`, and the agent card at `/.well-known/agent.json`. Includes health check endpoint and graceful shutdown handling.

---

### openai

An agent configured to use OpenAI as the LLM provider instead of the default Gemini.

**Command:**

```bash
cargo adk new my-agent --template openai
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
├── .env.example
└── README.md
```

**Description:** Identical structure to the `basic` template but configured with the OpenAI provider. The generated `Cargo.toml` enables the `openai` feature flag, and `main.rs` initializes an OpenAI model (GPT-4o by default). The `.env.example` includes `OPENAI_API_KEY` instead of `GOOGLE_API_KEY`.

---

### a2a

An Agent-to-Agent protocol server with simplified scaffolding using `A2aServer::quick_start`.

**Command:**

```bash
cargo adk new my-agent --template a2a
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   └── agent.rs
├── agent-card.json
├── .env.example
└── README.md
```

**Description:** Generates a production-ready A2A server using the simplified `A2aServer::quick_start` API introduced in v0.9.2. Includes a pre-configured agent card, session management, and streaming task support. The agent is immediately discoverable by other A2A-compatible agents on the network.

---

### graph

A graph-based workflow agent with checkpoints and durable execution.

**Command:**

```bash
cargo adk new my-agent --template graph
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── workflow.rs
│   └── nodes.rs
├── .env.example
└── README.md
```

**Description:** Generates a graph-based workflow using `adk-graph`. The `workflow.rs` module defines the graph topology with nodes and edges, while `nodes.rs` implements individual processing nodes. Includes checkpoint configuration for durable execution and automatic resume after failures.

---

### realtime

A real-time bidirectional audio/video streaming agent.

**Command:**

```bash
cargo adk new my-agent --template realtime
```

**Generated project structure:**

```
my-agent/
├── Cargo.toml
├── src/
│   ├── main.rs
│   └── handler.rs
├── .env.example
└── README.md
```

**Description:** Generates a real-time voice agent using `adk-realtime`. The `handler.rs` module implements the `EventHandler` trait for processing audio events. Configured for Gemini Live by default (switchable to OpenAI Realtime). Includes audio format configuration and interruption detection setup.

---

## Addons

Addons are cross-cutting capabilities that can be composed with any base template. Each addon injects the necessary dependencies, imports, initialization code, and configuration into your project.

Use the `--addon` flag to include one or more addons:

```bash
cargo adk new my-agent --template basic --addon telemetry --addon auth
```

### telemetry

**Description:** OpenTelemetry integration for distributed tracing and metrics collection.

**What it adds:**
- `adk-telemetry` dependency with OTLP exporter
- Tracing subscriber initialization in `main.rs`
- `OTEL_EXPORTER_OTLP_ENDPOINT` in `.env.example`
- Span instrumentation on agent execution

---

### auth

**Description:** Authentication and authorization support with API keys, JWT validation, and OAuth2.

**What it adds:**
- `adk-auth` dependency with auth middleware
- API key validation setup in `main.rs`
- `AUTH_API_KEY` and `JWT_SECRET` in `.env.example`
- Request context extraction for role-based access control

---

### eval

**Description:** Evaluation framework for testing agent quality with trajectory and semantic scoring.

**What it adds:**
- `adk-eval` dependency
- `tests/eval_tests.rs` with example evaluation harness
- Trajectory evaluator and semantic similarity scorer setup
- `#[ignore]` test annotations (requires API keys to run)

---

### docker

**Description:** Docker containerization with multi-stage build for minimal production images.

**What it adds:**
- `Dockerfile` with multi-stage Rust build (builder + runtime)
- `.dockerignore` excluding target/, .env, and .git/
- `docker-compose.yml` for local development with environment variables
- Health check configuration in the container

---

### ci

**Description:** Continuous integration pipeline configuration for GitHub Actions.

**What it adds:**
- `.github/workflows/ci.yml` with fmt, clippy, test, and build jobs
- Caching for Cargo registry and target directory
- Matrix strategy for multiple Rust versions
- Optional deployment step (commented out)

---

### monitoring

**Description:** Runtime monitoring with health checks, readiness probes, and metrics endpoints.

**What it adds:**
- `/health` and `/ready` HTTP endpoints
- Prometheus metrics endpoint at `/metrics`
- Custom agent execution metrics (latency, token usage, error rates)
- Graceful degradation reporting

---

### tracing

**Description:** Structured logging with `tracing` crate integration and configurable log levels.

**What it adds:**
- `tracing` and `tracing-subscriber` dependencies
- Environment-based log level configuration (`RUST_LOG`)
- JSON log formatting option for production
- Request ID propagation across async boundaries

---

### logging

**Description:** File-based logging with rotation and configurable output formats.

**What it adds:**
- `tracing-appender` dependency for file output
- Log rotation configuration (daily, by size)
- Separate error log file
- Console + file dual output setup

---

### testing

**Description:** Test infrastructure with property-based testing and integration test scaffolding.

**What it adds:**
- `proptest` dev-dependency
- `tests/` directory with unit and property test examples
- Test utilities module for common test helpers
- Mock provider setup for offline testing

---

## Enterprise Patterns

Enterprise patterns are pre-composed combinations of a base template and curated addons designed for specific production scenarios. They represent best practices for common deployment architectures.

Use the `--pattern` flag:

```bash
cargo adk new my-agent --pattern microservices
```

### microservices

**Description:** A microservice-ready agent with HTTP API, health checks, containerization, and observability.

**Base template:** api
**Included addons:** telemetry, monitoring, docker, ci

**Use cases:**
- Deploying agents as individual microservices in Kubernetes
- Building agent fleets with independent scaling
- Production deployments requiring health checks and metrics
- Teams using container orchestration (Docker Swarm, ECS, K8s)

**Command:**

```bash
cargo adk new order-processor --pattern microservices
```

---

### event-driven

**Description:** An event-driven agent that processes messages from queues or streams with durable execution.

**Base template:** graph
**Included addons:** telemetry, monitoring, logging

**Use cases:**
- Processing events from message queues (SQS, Kafka, NATS)
- Building reactive agent pipelines triggered by external events
- Long-running workflows with checkpoint-based recovery
- Systems requiring audit trails and event sourcing

**Command:**

```bash
cargo adk new event-handler --pattern event-driven
```

---

### multi-agent

**Description:** A multi-agent system with supervisor orchestration, shared state, and full observability.

**Base template:** basic
**Included addons:** telemetry, tracing, monitoring

**Use cases:**
- Building agent teams with specialized roles (researcher, writer, reviewer)
- Orchestrating complex workflows across multiple agents
- Systems requiring detailed execution traces for debugging
- Research and experimentation with multi-agent architectures

**Command:**

```bash
cargo adk new research-team --pattern multi-agent
```

---

### serverless

**Description:** A lightweight agent optimized for serverless deployment with minimal cold start time.

**Base template:** basic
**Included addons:** telemetry, logging

**Use cases:**
- AWS Lambda, Google Cloud Functions, or Azure Functions deployment
- Cost-optimized agents that scale to zero
- Event-triggered agents with infrequent invocations
- Environments with strict binary size constraints

**Command:**

```bash
cargo adk new lambda-agent --pattern serverless
```

---

### data-pipeline

**Description:** A sequential data processing pipeline with session state persistence and evaluation.

**Base template:** basic
**Included addons:** telemetry, eval, logging, testing

**Use cases:**
- ETL pipelines with AI-powered transformation steps
- Document processing workflows (ingest → analyze → summarize → store)
- Batch processing with quality evaluation gates
- Data enrichment pipelines with LLM-based classification

**Command:**

```bash
cargo adk new doc-processor --pattern data-pipeline
```

---

## Usage

### The `--addon` flag

The `--addon` flag adds a capability addon to any base template. You can specify multiple addons by repeating the flag:

```bash
# Single addon
cargo adk new my-agent --template basic --addon telemetry

# Multiple addons
cargo adk new my-agent --template tools --addon telemetry --addon auth --addon docker

# Addons with enterprise-ready templates
cargo adk new my-agent --template a2a --addon monitoring --addon ci
```

**Addon ordering:** Addons are applied in priority order regardless of the order specified on the command line. This ensures initialization code appears in the correct sequence (e.g., tracing is initialized before auth middleware).

### Combining templates with multiple addons

Any base template can be combined with any compatible addon. Here are common combinations:

```bash
# Production API server with full observability
cargo adk new prod-api --template api --addon auth --addon telemetry --addon monitoring --addon docker

# RAG agent with evaluation and testing
cargo adk new smart-search --template rag --addon eval --addon testing

# Real-time voice agent with logging and CI
cargo adk new voice-bot --template realtime --addon logging --addon ci

# Graph workflow with full production stack
cargo adk new workflow --template graph --addon telemetry --addon monitoring --addon docker --addon ci
```

### Using enterprise patterns with additional addons

Enterprise patterns can be further extended with additional addons:

```bash
# Microservices pattern + auth
cargo adk new secure-service --pattern microservices --addon auth

# Multi-agent pattern + evaluation
cargo adk new eval-team --pattern multi-agent --addon eval --addon testing
```

## Examples

### Example 1: Quick prototype

Create a minimal agent for rapid prototyping:

```bash
cargo adk new prototype --template basic
cd prototype
echo "GOOGLE_API_KEY=your-key-here" > .env
cargo run
```

### Example 2: Production-ready API

Create a fully instrumented API server ready for deployment:

```bash
cargo adk new my-service --template api --addon auth --addon telemetry --addon monitoring --addon docker --addon ci
cd my-service
# Edit .env.example values
cp .env.example .env
# Build and run locally
cargo run
# Or build the Docker image
docker build -t my-service .
```

### Example 3: Multi-agent research system

Create a multi-agent system with evaluation:

```bash
cargo adk new research-system --pattern multi-agent --addon eval
cd research-system
cargo run -- "Research the latest advances in quantum computing"
```

### Example 4: RAG-powered knowledge base

Create a retrieval-augmented agent with testing infrastructure:

```bash
cargo adk new knowledge-base --template rag --addon testing --addon telemetry
cd knowledge-base
# Add documents to data/ directory
cargo run -- "What does our documentation say about error handling?"
```

### Example 5: Listing available templates

View all available templates, addons, and patterns:

```bash
# List all templates
cargo adk new --list

# Show details for a specific template
cargo adk new --template tools --info

# Show compatible addons for a template
cargo adk new --template graph --list-addons
```
