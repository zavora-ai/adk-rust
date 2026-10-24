# Realtime voice

> **Note:** `/ui/` answers `404`: `adk-server` no longer bundles a runtime UI (spec 3.3 §10
> S1). Send prompts through the REST API the runtime serves under `http://127.0.0.1:8088/api/`
> (`POST /api/run_sse` or `POST /api/run`; see
> [Server API](../../../docs/official_docs/deployment/server.md)).

`voice_assistant` is a real `RealtimeAgent` backed by OpenAI Realtime. Send to it
(`appName` `voice_assistant`, `POST /api/run_sse`):

> Give me a calm, twenty-second breathing exercise.

The run's events stream the response transcript and carry the completed PCM
audio. A realtime session remains open for additional bidirectional input, so
close the stream after the first response in this text-seeded demonstration.

The runtime seeds the realtime session with text. The
full microphone/WebSocket conversation surface remains available in
[`examples/realtime_voice`](../../realtime_voice/README.md); it is intentionally
not simulated here.
