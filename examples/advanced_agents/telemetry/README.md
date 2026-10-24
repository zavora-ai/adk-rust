# Telemetry and runtime services

> **Note:** `/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10
> S1). Send prompts through the REST API the runtime serves under `http://127.0.0.1:8088/api/`
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../../docs/official_docs/deployment/server.md)).

The advanced runtime initializes `AdkSpanExporter` and configures in-memory
artifact and memory services. Agent details report whether each service is
actually configured. Telemetry reports `configured` before the first retained
span and `collecting` after the runtime captures one; console `RUST_LOG` levels
do not disable this collection.

After any interactive, ambient, realtime, A2A, or MCP-backed run:

1. `GET /api/debug/trace/session/{session_id}` for session spans, durations,
   trace IDs, and attributes;
2. `GET /api/apps/{app_name}/users/{user_id}/sessions/{session_id}` for the
   ordered ADK events and the session state;
3. `GET /api/sessions/{app_name}/{user_id}/{session_id}/artifacts` for the
   session's artifacts;
4. `GET /api/ui/capabilities` and `GET /api/ui/agents/{name}` for server-service
   and protocol availability.

Separating telemetry from the event timeline keeps spans visible even for runs
with many streaming or tool events.
