# Agent Registry — discovery, registration, and remote invocation

The Agent Registry is the Govern pillar of the Gemini Enterprise Agent Platform: a project-scoped catalog of agents, MCP servers, and endpoints. ADK-Rust participates in three ways.

| Capability | Feature | Crate |
|------------|---------|-------|
| Search and resolve registered agents (`AgentRegistryClient`, `AgentSearchTool`) | `vertex-agent-registry` | `adk-tool` |
| Register self-hosted agents, MCP servers, and endpoints from the CLI | `vertex-agent-registry` | `adk-cli` |
| Invoke a registered Agent Engine as a sub-agent (`RemoteReasoningEngineAgent`) | `vertex-remote-engine` | `adk-server` |

> **Note:** these features are unrelated to the local YAML `agent-registry` feature on `adk-server`, and the Agent Registry (agents/MCP servers/endpoints) is a different platform service from the Skill Registry (SKILL.md packages — see `docs/official_docs/skills/skill-registry.md`).

## Automatic vs. manual registration

Agent Engine deployments **register themselves**: creating an engine (e.g. with `adk-rust deploy agent-engine`) adds it to the registry and keeps the entry lifecycle-synced. Do not register it again — a manual entry would duplicate it under a different URN namespace.

Manual registration exists for everything else, and that is what the CLI covers:

```bash
# A self-hosted adk-server agent, described by its A2A agent card
adk-rust registry register-agent --service-id my-agent --card agent-card.json

# An external MCP server (you supply the tools/list payload; no introspection)
adk-rust registry register-mcp --service-id my-mcp --tool-spec tools-list.json \
  --url https://mcp.example.com/rpc

# A bare governed endpoint
adk-rust registry register-endpoint --service-id my-api --url https://api.example.com/v1

# Discovery
adk-rust registry search "travel planner" --type agent
```

All commands take `--project`/`--location` (falling back to `GOOGLE_CLOUD_PROJECT`/`GOOGLE_CLOUD_LOCATION`) and are idempotent: re-registering an existing service ID patches only the changed fields and performs no write when nothing changed. Manual entries are **not** lifecycle-synced — updating or removing them stays your responsibility. The `us`/`eu` multi-regions do not support manual registration; use a region or `global`.

## Discovering agents from an agent

`AgentSearchTool` gives an `LlmAgent` registry search as an ordinary read-only tool:

```rust,no_run
use adk_tool::{AgentRegistryClient, AgentRegistryConfig, AgentSearchTool};
use std::sync::Arc;

# fn build() -> adk_core::Result<()> {
let registry = AgentRegistryClient::new_with_adc(
    AgentRegistryConfig::new("my-project", "us-central1"),
)?;
let search_tool = Arc::new(AgentSearchTool::new(Arc::new(registry)));
# let _ = search_tool;
# Ok(())
# }
```

The tool returns `{urn, displayName, description, skills, endpoint}` summaries across agents, MCP servers, or endpoints.

## Delegating to a deployed engine

`RemoteReasoningEngineAgent` makes any deployed Agent Engine an ordinary sub-agent — it forwards the turn over `reasoningEngines:streamQuery` and yields the remote events as ADK events:

```rust,no_run
use adk_server::agent_engine::remote::RemoteReasoningEngineAgent;

# async fn build() -> adk_core::Result<()> {
let remote = RemoteReasoningEngineAgent::builder("specialist")
    .resource_name("projects/my-project/locations/us-central1/reasoningEngines/4242")
    .build()
    .await?;
# let _ = remote;
# Ok(())
# }
```

Engines can also be addressed by registry URN (`.urn(...)` plus `.registry(...)`); the resource name is resolved from the entry's runtime reference. The default class method is `streaming_agent_run_with_events` with a one-shot fallback to `stream_query` for non-ADK engines.

See `examples/agent_orchestrator` for the full pattern — registry search plus remote delegation in one orchestrator.

## Enabling everything

```toml
[dependencies]
adk-rust = { version = "2.2.0", features = ["minimal", "gemini-agent-platform"] }
```

The `gemini-agent-platform` meta-feature includes `vertex-agent-registry` and `vertex-remote-engine` along with every other platform integration.
