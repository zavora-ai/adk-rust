# A2A exposure

> **Note:** `/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10
> S1). Send prompts through the REST API the runtime serves under `http://127.0.0.1:8088/api/`
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../../docs/official_docs/deployment/server.md)).

`a2a_gateway` is the server root and is exposed through both the normal runtime
API and ADK-Rust's A2A endpoints:

- `GET /.well-known/agent-card.json`
- `POST /a2a`
- `POST /a2a/stream`

The agent card names the A2A endpoint. The REST API and A2A execute the same
agent object, model, session service, callbacks, and telemetry configuration.

Example request:

```bash
curl http://127.0.0.1:8088/.well-known/agent-card.json | jq .
```
