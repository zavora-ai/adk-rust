- **MCP server status transitions and stdio launcher** (`adk-tool`):
  `McpServerManager::subscribe_status_transitions` delivers every `ServerStatus` change as a
  `ServerStatusTransition { id, from, to }` on a broadcast channel, in order, wherever the
  manager records it — starts, stops, restarts, the monitor's crash and auto-restart handling,
  updates, enable, disable and shutdown; `FailedToStart` after the restart policy's attempts is
  the exhausted state. `with_stdio_launcher` takes a
  `Fn(&McpServerConfig) -> Result<tokio::process::Command>` that builds each stdio server's
  command in place of the configured one, for example inside an OS sandbox; without it the
  configured command, arguments and environment are spawned as before. Spec 3.2 §10 T1.
