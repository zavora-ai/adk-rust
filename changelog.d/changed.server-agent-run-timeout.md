- **Agent runs get their own timeout** (`adk-server`): `/api/run`,
  `/api/run/{app}/{user}/{session}`, `/api/run_sse`, `/a2a`, and `/a2a/stream` answer `408` after
  `SecurityConfig::run_timeout` (default `DEFAULT_RUN_TIMEOUT`, 15 minutes;
  `ServerConfig::with_run_timeout`) instead of the 30-second `request_timeout`, which a model
  turn with tool calls routinely outlasted. Every other route keeps `request_timeout`.
