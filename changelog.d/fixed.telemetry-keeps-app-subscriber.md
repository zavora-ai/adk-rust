- **Serving keeps the application's tracing subscriber** (`adk-telemetry`, `adk-cli`):
  `init_with_adk_exporter` returns `TelemetryError::Init` instead of panicking when a global
  subscriber is already installed, so a `HarnessLauncher` or `Launcher` app that sets up
  `tracing_subscriber` in `main` — as every `cargo adk` template does — starts `serve` and
  `up`; the server logs that the in-process exporter is off.
