- **`adk_cli::serve::run_serve` logs its listening address through `tracing`** (`adk-cli`): the
  "ADK Server starting on …" and "Press Ctrl+C to stop" lines printed to stdout are replaced by one
  `info!` event (`server.address`), so the address appears only when a `tracing` subscriber is
  installed. The `adk-rust serve` binary and the `Launcher` console keep their terminal output.
