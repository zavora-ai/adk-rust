- **Telemetry init keeps the application's tracing subscriber** (`adk-telemetry`, `adk-cli`):
  `init_telemetry`, `init_with_otlp`, `init_with_sqlite`, `init_with_adk_exporter`,
  `init_with_gcp` and `init_json_logging` return `TelemetryError::Init` instead of panicking
  when a global subscriber is already installed, and leave it in place. A `HarnessLauncher` or
  `Launcher` app that sets up `tracing_subscriber` in `main` — as every `cargo adk` template
  does — now starts `serve` and `up`; the server logs that the in-process exporter is off.
