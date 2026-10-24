# Ambient scheduling

> **Note:** `/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10
> S1). Send prompts through the REST API the runtime serves under `http://127.0.0.1:8088/api/`
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../../docs/official_docs/deployment/server.md)).

`ambient_monitor` both answers interactive runs over the REST API and is driven in
the background by an `AmbientAgent`. A cron trigger fires every 30 seconds and
uses `RunnerTriggerConfig` to invoke the exact same root through `Runner`.
Override `ADVANCED_AMBIENT_CRON` with another six-field schedule when needed.

The trigger uses `local-user` and a shared `ambient-monitor` session, so the session
API finds it without changing identity.

Expected behavior:

1. Wait for the first trigger.
2. `GET /api/apps/ambient_monitor/users/local-user/sessions/ambient-monitor` returns
   the session, with the generated operational pulse in its events.
3. `GET /api/debug/trace/session/ambient-monitor` returns the corresponding
   Runner and model spans.

The shared session is deliberate for the demo. Production schedules should
prefer `TriggerSessionPolicy::PerTrigger` unless accumulated history is needed.
