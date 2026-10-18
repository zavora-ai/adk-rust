- **`McpServerManager` starts servers without holding its registry lock**
  (`adk-tool`): a server is `ServerStatus::Starting` (a new variant) while its child
  completes the MCP handshake, and the handshake is bounded by
  `with_startup_timeout` (default 30 seconds) after which the server is
  `FailedToStart`. A server that never answered `initialize` held the write lock
  forever, blocking every other server's tools, status and lifecycle calls.
  `start_all` now runs the handshakes concurrently, and stopping, updating, disabling
  or removing a `Starting` server cancels its handshake. Exhaustive matches on
  `ServerStatus` need a `Starting` arm.
