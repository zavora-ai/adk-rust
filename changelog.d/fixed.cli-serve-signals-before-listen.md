- **`adk-rust serve` and `Launcher::run_serve_directly` stop cleanly on an early signal**
  (`adk-cli`): Ctrl-C and `SIGTERM` were caught only once the server first polled
  `adk_server::shutdown_signal`, after the address listened, so a signal sent as soon as a
  client could connect ended the process by its default action. `adk-rust serve` (harness and
  `--agent chat`), `Launcher::run_serve_directly` and `adk_cli::serve::run_serve` now catch
  both before the address listens, as `adk-rust up` does, and drain the server.
