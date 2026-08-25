# Agent Engine (Gemini Enterprise Agent Platform)

The `agent-engine` feature makes an ADK-Rust agent drivable by the Gemini
Enterprise Agent Platform as a custom-container ReasoningEngine. A container
running [`serve_agent_engine`] answers `reasoningEngines.query`,
`reasoningEngines.streamQuery`, the console Playground, and the platform
SDKs — the same runtime contract adk-python's `AdkApp` implements.

## Overview

The platform drives a deployed engine through two container endpoints:

| Endpoint | Mode | Response |
|----------|------|----------|
| `POST /api/reasoning_engine` | unary | `{"output": ...}` |
| `POST /api/stream_reasoning_engine` | streaming | one JSON object per line (`Content-Type: application/json`, no SSE framing) |

Both take the dispatch envelope `{"class_method": "...", "input": {...}}`.
The envelope is snake_case — the platform dispatches on Python method names.
The turnkey app also serves `GET /health` for container health checks.

## Quick start

Enable the feature (it is included in the `gemini-agent-platform`
meta-feature):

```toml
[dependencies]
adk-rust = { version = "2.1.0", features = ["minimal", "agent-engine"] }
```

`serve_agent_engine` is the whole `main` of a deployable engine. It binds
`0.0.0.0:$PORT` (fallback `8080`) and serves until stopped:

```rust
use adk_rust::prelude::*;
use adk_server::agent_engine::{AgentEngineOptions, serve_agent_engine};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Answers weather questions")
        .instruction("You are a helpful weather assistant.")
        .model(model)
        .build()?;

    serve_agent_engine(Arc::new(agent), AgentEngineOptions::new()).await
}
```

Verify with the codelab's payload:

```bash
curl -s -X POST localhost:8080/api/stream_reasoning_engine \
  -H 'Content-Type: application/json' \
  -d '{"class_method": "async_stream_query", "input": {"user_id": "u", "message": "hi"}}'
```

Each response line is one ADK event as JSON.

## Operations

The engine registers the exact operation set adk-python's `AdkApp`
advertises. Sync/async name pairs map to the same handler — the split is a
Python artifact the wire contract preserves.

| `class_method` | API mode | Behavior |
|---|---|---|
| `create_session`, `async_create_session` | `""` / `async` | Create a session (optional caller-chosen ID and initial state) |
| `get_session`, `async_get_session` | `""` / `async` | Fetch a session with its events |
| `list_sessions`, `async_list_sessions` | `""` / `async` | List a user's sessions |
| `delete_session`, `async_delete_session` | `""` / `async` | Delete a session |
| `stream_query`, `async_stream_query` | `stream` / `async_stream` | Run the agent; the session is created automatically when absent |
| `streaming_agent_run_with_events` | `async_stream` | Run the agent from an `AgentRunRequest` JSON string (console Playground path) |
| `async_add_session_to_memory` | `async` | Extract a session's events into the configured memory service |
| `async_search_memory` | `async` | Search the configured memory service |
| `register_operations` | `""` | Advertise this table to the host |

Unknown class methods return `400` with a problem-JSON body. The memory
methods return an `Unsupported` error (`501`) until a memory service is
configured.

> **Note:** `reasoningEngines:asyncQuery` (durable query jobs) is not
> registered: the capability must be declared at engine create time, cannot
> be added post-create, and adk-python's `AdkApp` does not register it
> either.

## Managed backends

The zero-configuration default keeps sessions in memory — enough to answer
queries, but conversations do not survive a container restart. Deployed
engines configure managed backends through `AgentEngineOptions`.

### Managed sessions (Vertex AI Sessions)

With the `vertex-session` feature, `VertexAiSessionConfig::from_env()` reads
the variables the platform sets inside deployed containers
(`GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
`GOOGLE_CLOUD_AGENT_ENGINE_ID` — the bare numeric engine ID):

```rust
use adk_server::agent_engine::AgentEngineOptions;
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};
use std::sync::Arc;

fn managed_sessions() -> adk_core::Result<AgentEngineOptions> {
    let config = VertexAiSessionConfig::from_env()?;
    let sessions = Arc::new(VertexAiSessionService::new_with_adc(config)?);
    Ok(AgentEngineOptions::new().with_session_service(sessions))
}
```

Outside a deployed container, construct the config explicitly with
`VertexAiSessionConfig::new(project, location).with_reasoning_engine(id)`.

### Artifacts (Google Cloud Storage)

With the `gcs-artifacts` feature, `GcsArtifactService` stores artifacts in
the blob layout the Gemini Enterprise console reads (byte-for-byte parity
with adk-python). Take the bucket from an environment variable or a flag:

```rust
use adk_artifact::GcsArtifactService;
use adk_server::agent_engine::AgentEngineOptions;
use std::sync::Arc;

fn gcs_artifacts() -> adk_core::Result<AgentEngineOptions> {
    let bucket = std::env::var("ADK_ARTIFACT_BUCKET").unwrap_or_else(|_| "my-bucket".to_string());
    let artifacts = Arc::new(GcsArtifactService::new_with_adc(bucket)?);
    Ok(AgentEngineOptions::new().with_artifact_service(artifacts))
}
```

The artifact service is wired into both the runner (tool-facing saves and
loads) and the dispatch state.

### Memory

`AgentEngineOptions::with_memory_service` accepts any
`adk_memory::MemoryService` and enables the two memory class methods. The
platform's Memory Bank backend arrives with the `vertex-memory` feature in a
later release.

## ServerBuilder integration

An existing ADK server can expose the dispatch surface alongside its REST,
UI, and A2A routes:

```rust
use adk_server::{ServerBuilder, ServerConfig};

fn build_app(config: ServerConfig) -> axum::Router {
    ServerBuilder::new(config).with_agent_engine(true).build()
}
```

The dispatch routes serve the loader's root agent with the configured
session and artifact services. They do **not** carry the server's auth
middleware: a deployed engine is fronted by the platform, which
authenticates callers before they reach the container. Do not expose these
endpoints directly to untrusted networks.

## Deploying from the CLI

With `adk-cli` installed with the `gcp-deploy` feature
(`cargo install adk-cli --features gcp-deploy`), one command creates the
engine from a pushed container image:

```bash
# 1. Build and push the image
gcloud builds submit --tag us-central1-docker.pkg.dev/PROJECT/agents/my-agent:latest

# 2. Deploy it as a ReasoningEngine
adk-rust deploy agent-engine \
  --image-uri us-central1-docker.pkg.dev/PROJECT/agents/my-agent:latest \
  --project PROJECT \
  --location us-central1 \
  --service-account agent-runner@PROJECT.iam.gserviceaccount.com
```

Optional flags: `--display-name` (defaults to the image name) and
`--kms-key` for CMEK. The command declares the full class-method contract
from the [operations table](#operations), waits for the create operation,
and prints the engine resource name. The same client is available
programmatically as `adk_deploy::gcp::GcpDeployClient` (umbrella feature
`gcp-deploy`).

## Environment variables

| Variable | Meaning |
|----------|---------|
| `PORT` | Serving port assigned by the platform (fallback `8080`; an invalid value fails startup) |
| `GOOGLE_CLOUD_PROJECT` | GCP project of the deployment |
| `GOOGLE_CLOUD_LOCATION` | GCP location of the deployment |
| `GOOGLE_CLOUD_AGENT_ENGINE_ID` | Bare numeric engine ID, set inside deployed containers |

When `GOOGLE_CLOUD_AGENT_ENGINE_ID` is present and the session service is
the in-memory default, the entrypoint logs a warning: deployed engines
should use managed sessions.

[`serve_agent_engine`]: https://docs.rs/adk-server/latest/adk_server/agent_engine/fn.serve_agent_engine.html
