- **`McpServerManager` bounds server startup and reports `ServerStatus::Starting`**
  (`adk-tool`): `start_server` held the registry write lock across the spawn and MCP
  handshake, so a server that never answered `initialize` blocked every other server's
  tools, status, and lifecycle calls. The lock is now released during the handshake, which
  `with_startup_timeout` bounds (default `DEFAULT_STARTUP_TIMEOUT`, 30 seconds); a server
  that has not finished by then is `FailedToStart`. Meanwhile the server is
  `ServerStatus::Starting`, a new variant, so exhaustive matches on `ServerStatus` need a
  `Starting` arm. `start_all` runs the handshakes concurrently, and stopping, updating,
  disabling, or removing a `Starting` server cancels its handshake. A server that needs longer
  to start, such as an `npx` package downloaded on first run, needs a larger
  `with_startup_timeout`.
