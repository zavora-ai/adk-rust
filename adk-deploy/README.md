# adk-deploy

Deployment manifest, bundling, and control-plane client for ADK-Rust agents.

[![Crates.io](https://img.shields.io/crates/v/adk-deploy.svg)](https://crates.io/crates/adk-deploy)
[![Documentation](https://docs.rs/adk-deploy/badge.svg)](https://docs.rs/adk-deploy)
[![License](https://img.shields.io/crates/l/adk-deploy.svg)](LICENSE)

## Overview

`adk-deploy` provides everything needed to package and deploy ADK-Rust agents to a control plane:

- **Deployment manifests** — TOML-based configuration covering agent identity, build settings, scaling, health checks, deployment strategy, service bindings, secrets, telemetry, auth, guardrails, realtime, A2A, graph/HITL, plugins, skills, and interaction triggers
- **Bundle builder** — compiles the agent binary, packages it with assets into a `.tar.gz` archive, and generates SHA-256 integrity checksums
- **Control-plane client** — HTTP client for push deployments, status checks, rollbacks, promotions, secret management, and dashboard queries
- **Comprehensive validation** — manifests are validated before build or push (unique bindings, secret refs, auth mode consistency, graph checkpoint requirements, trigger field completeness)

## Installation

```toml
[dependencies]
adk-deploy = "0.5.0"
```

## Manifest Format

Create an `adk-deploy.toml` in your project root:

```toml
[agent]
name = "my-agent"
binary = "my-agent"
version = "1.0.0"
description = "A production AI agent"

[build]
profile = "release"
features = ["openai", "tools"]
assets = ["prompts/", "config.yaml"]

[scaling]
minInstances = 2
maxInstances = 20
targetLatencyMs = 300
targetCpuPercent = 70

[health]
path = "/api/health"
intervalSecs = 10
timeoutSecs = 5
failureThreshold = 3

[strategy]
type = "rolling"
# type = "canary"
# trafficPercent = 10
# type = "blue-green"

[[services]]
name = "sessions"
kind = "postgres"
mode = "managed"

[[services]]
name = "memory"
kind = "pgvector"
mode = "external"
secretRef = "PGVECTOR_URL"

[[secrets]]
key = "OPENAI_API_KEY"
required = true

[[secrets]]
key = "PGVECTOR_URL"
required = true

[env]
LOG_LEVEL = "info"
OPENAI_API_KEY = { secretRef = "OPENAI_API_KEY" }

[telemetry]
otlpEndpoint = "https://otel.example.com:4317"
serviceName = "my-agent"

[auth]
mode = "bearer"
requiredScopes = ["agent:invoke"]

[guardrails]
piiRedaction = true
contentFilters = ["toxicity"]

[a2a]
enabled = true

[interaction.manual]
inputLabel = "Ask the agent"
defaultPrompt = "What can you help me with?"

[[interaction.triggers]]
id = "daily-report"
name = "Daily Report"
kind = "schedule"
cron = "0 9 * * *"
timezone = "America/New_York"
```

## Usage

### Loading and Validating a Manifest

```rust
use adk_deploy::DeploymentManifest;
use std::path::Path;

let manifest = DeploymentManifest::from_path(Path::new("adk-deploy.toml"))?;
println!("Agent: {} v{}", manifest.agent.name, manifest.agent.version);
```

### Building a Bundle

```rust
use adk_deploy::{BundleBuilder, DeploymentManifest};
use std::path::Path;

let manifest_path = Path::new("adk-deploy.toml");
let manifest = DeploymentManifest::from_path(manifest_path)?;
let builder = BundleBuilder::new(manifest_path, manifest);

let artifact = builder.build()?;
println!("Bundle: {}", artifact.bundle_path.display());
println!("SHA-256: {}", artifact.checksum_sha256);
```

The bundle builder:
1. Validates the manifest
2. Runs `cargo build` with the configured profile, target, and features
3. Packages the binary + manifest + assets into a `.tar.gz`
4. Writes a `.sha256` checksum file alongside the archive

### Pushing a Deployment

```rust
use adk_deploy::{DeployClient, DeployClientConfig, PushDeploymentRequest};

let config = DeployClientConfig::load()?;
let client = DeployClient::new(config);

let response = client.push_deployment(&PushDeploymentRequest {
    workspace_id: Some("ws-123".into()),
    environment: "production".into(),
    manifest: manifest.clone(),
    bundle_path: artifact.bundle_path.to_string_lossy().into(),
    checksum_sha256: artifact.checksum_sha256.clone(),
    binary_path: None,
}).await?;

println!("Deployed: {}", response.deployment.id);
```

### Deployment Operations

```rust
// Check status
let status = client.status("production", Some("my-agent")).await?;

// View history
let history = client.history("production", Some("my-agent")).await?;

// Rollback
let rolled_back = client.rollback("deploy-id-123").await?;

// Promote (canary → full)
let promoted = client.promote("deploy-id-123").await?;

// Dashboard overview
let dashboard = client.dashboard().await?;
```

### Secret Management

```rust
use adk_deploy::SecretSetRequest;

// Set a secret
client.set_secret(&SecretSetRequest {
    environment: "production".into(),
    key: "OPENAI_API_KEY".into(),
    value: "sk-...".into(),
}).await?;

// List secrets (keys only, values never returned)
let secrets = client.list_secrets("production").await?;

// Delete a secret
client.delete_secret("production", "OLD_KEY").await?;
```

## Manifest Sections

| Section | Purpose |
|---------|---------|
| `agent` | Name, binary, version, description |
| `build` | Profile, target triple, features, system deps, assets |
| `scaling` | Min/max instances, latency/CPU/concurrency targets |
| `health` | Health check path, interval, timeout, failure threshold |
| `strategy` | Rolling, blue-green, or canary with traffic percentage |
| `services` | Service bindings (postgres, redis, sqlite, mongodb, neo4j, pgvector, MCP, checkpoints) |
| `secrets` | Secret key declarations with required flag |
| `env` | Environment variables (plain values or secret refs) |
| `telemetry` | OTLP endpoint, service name, resource attributes |
| `auth` | Auth mode (disabled/bearer/OIDC), scopes, issuer, JWKS |
| `guardrails` | PII redaction, content filters |
| `realtime` | Realtime features (openai, gemini, vertex-live, livekit) |
| `a2a` | Agent-to-Agent protocol toggle |
| `graph` | Checkpoint binding, HITL toggle |
| `plugins` | Plugin references |
| `skills` | Skills directory, hot reload |
| `interaction` | Manual input config, webhook/schedule/event triggers |
| `source` | Source metadata (Studio project ID, etc.) |

## Service Binding Kinds

| Kind | Description |
|------|-------------|
| `in-memory` | In-memory (dev/test only) |
| `postgres` | PostgreSQL sessions |
| `redis` | Redis sessions |
| `sqlite` | SQLite sessions |
| `mongo-db` | MongoDB sessions |
| `neo4j` | Neo4j sessions |
| `pgvector` | PostgreSQL + pgvector memory |
| `redis-memory` | Redis memory |
| `mongo-memory` | MongoDB memory |
| `neo4j-memory` | Neo4j memory |
| `artifact-storage` | Binary artifact storage |
| `mcp-server` | MCP server connection |
| `checkpoint-postgres` | Graph checkpoint (PostgreSQL) |
| `checkpoint-redis` | Graph checkpoint (Redis) |

## Deployment Strategies

| Strategy | Description |
|----------|-------------|
| `rolling` | Gradual instance replacement (default) |
| `blue-green` | Full parallel deployment, instant switch |
| `canary` | Route `trafficPercent` to new version, promote or rollback |

## Error Handling

All operations return `DeployResult<T>` with structured `DeployError` variants:

```rust
use adk_deploy::DeployError;

match result {
    Err(DeployError::ManifestNotFound { path }) => { /* file missing */ }
    Err(DeployError::InvalidManifest { message }) => { /* validation failed */ }
    Err(DeployError::BundleBuild { message }) => { /* cargo build failed */ }
    Err(DeployError::Client { message }) => { /* HTTP request failed */ }
    Err(DeployError::Config { message }) => { /* config persistence failed */ }
    _ => {}
}
```

## Client Configuration

The client config is stored at `~/.config/adk-deploy/config.json`:

```json
{
  "endpoint": "https://deploy.example.com",
  "token": "...",
  "workspaceId": "ws-123"
}
```

Load with `DeployClientConfig::load()` (defaults to `http://127.0.0.1:8090` if no config exists).

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — Umbrella crate
- [adk-server](https://crates.io/crates/adk-server) — REST API and A2A server
- [adk-cli](https://crates.io/crates/adk-cli) — CLI with `deploy` subcommands

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
