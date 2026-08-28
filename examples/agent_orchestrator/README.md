# Agent Orchestrator

An `LlmAgent` that participates in the Gemini Enterprise Agent Platform's Govern pillar: it discovers registered agents with `AgentSearchTool` (Agent Registry) and delegates work to a deployed Agent Engine agent through `RemoteReasoningEngineAgent` (`reasoningEngines:streamQuery`).

## Prerequisites

- Application Default Credentials: `gcloud auth application-default login`
- A deployed Agent Engine speaking the ADK class-method contract — for example one created with `adk-rust deploy agent-engine` (see `docs/official_docs/deployment/agent-engine.md`)

## Environment

| Variable | Purpose |
|----------|---------|
| `GOOGLE_API_KEY` | Gemini model for the orchestrator itself |
| `GOOGLE_CLOUD_PROJECT` | Registry + engine project |
| `GOOGLE_CLOUD_LOCATION` | Registry + engine region (e.g. `us-central1`) |
| `VERTEX_REMOTE_ENGINE` | Full `projects/*/locations/*/reasoningEngines/*` name of the agent to delegate to |

## Run

```bash
cargo run --manifest-path examples/agent_orchestrator/Cargo.toml
```

The orchestrator lists what the registry knows, then forwards the substantive question to the remote agent and streams its events back as ordinary ADK events.
