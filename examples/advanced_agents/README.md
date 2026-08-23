# Advanced agents in the ADK Runtime

This crate runs four different ADK architectures through one server and one
embedded interface:

| Agent | What it demonstrates | UI evidence |
|---|---|---|
| `a2a_gateway` | The same OpenAI agent exposed through interactive SSE and A2A | Protocols tab links to the live agent card |
| `ambient_monitor` | `AmbientAgent` + `CronTrigger` + `RunnerTriggerConfig` | Background sessions appear automatically with events and spans |
| `voice_assistant` | `RealtimeAgent` using OpenAI Realtime | Transcript streaming and a playable WAV response |
| `mcp_warehouse` | MCP `2026-07-28` discovery and SEP-2663 tasks | Tool calls/results in the transcript; MCP Apps capabilities in Protocols |

Telemetry, in-memory artifacts, and cross-session memory are enabled for the
server. The dedicated **Telemetry** tab shows spans separately from runtime
events, while **Protocols** reports those services, UI protocol revisions, MCP
Apps features, and A2A discovery.

## Run

```bash
cp examples/advanced_agents/.env.example .env
# Set OPENAI_API_KEY in .env.

cargo build --manifest-path examples/advanced_agents/Cargo.toml --bins
cargo run --manifest-path examples/advanced_agents/Cargo.toml \
  --bin advanced-runtime
```

Open `http://127.0.0.1:8088/ui/`.

## Walkthroughs

- [Ambient scheduling](ambient/README.md)
- [Realtime voice](realtime/README.md)
- [A2A exposure](a2a/README.md)
- [MCP discovery and tasks](mcp/README.md)
- [Telemetry and runtime services](telemetry/README.md)

The examples compile in CI without contacting OpenAI. Network calls begin only
after the runtime binary is launched and a scheduled or interactive run starts.
