# Advanced agents in the ADK Runtime

This crate runs four different ADK architectures through one server and its REST,
SSE and A2A APIs:

| Agent | What it demonstrates | What the API shows |
|---|---|---|
| `a2a_gateway` | The same OpenAI agent exposed through interactive SSE and A2A | `GET /.well-known/agent-card.json` serves the live agent card |
| `ambient_monitor` | `AmbientAgent` + `CronTrigger` + `RunnerTriggerConfig` | Background sessions appear in the session API with their events and spans |
| `voice_assistant` | `RealtimeAgent` using OpenAI Realtime | The run's events stream the transcript and carry the response audio |
| `mcp_warehouse` | MCP `2026-07-28` discovery and SEP-2663 tasks | Tool calls and results in the run's events; MCP Apps capabilities in `GET /api/ui/capabilities` |

Telemetry, in-memory artifacts, and cross-session memory are enabled for the
server. `GET /api/debug/trace/session/{session_id}` returns spans separately from
runtime events, while `GET /api/ui/capabilities` and `GET /api/ui/agents/{name}`
report those services, UI protocol revisions, MCP Apps features, and A2A discovery.

## Run

```bash
cp examples/advanced_agents/.env.example .env
# Set OPENAI_API_KEY in .env.

cargo build --manifest-path examples/advanced_agents/Cargo.toml --bins
cargo run --manifest-path examples/advanced_agents/Cargo.toml \
  --bin advanced-runtime
```

The runtime serves its REST, SSE and A2A APIs on `http://127.0.0.1:8088/api/`.

> **Note:** `adk-server` no longer bundles a runtime UI (spec 3.3 §10 S1). The web home of
> `adk-surface` replaces it, and these binaries serve the REST, SSE and A2A APIs, so `/ui/`
> answers `404` on them. The walkthroughs send their prompts through the REST API
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../docs/official_docs/deployment/server.md)). See
> [The Web Home](../../docs/official_docs/surfaces/web-home.md).

## Walkthroughs

- [Ambient scheduling](ambient/README.md)
- [Realtime voice](realtime/README.md)
- [A2A exposure](a2a/README.md)
- [MCP discovery and tasks](mcp/README.md)
- [Telemetry and runtime services](telemetry/README.md)

The examples compile in CI without contacting OpenAI. Network calls begin only
after the runtime binary is launched and a scheduled or interactive run starts.
