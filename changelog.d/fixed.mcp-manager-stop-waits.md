- **`McpServerManager` waits for a stopped server to exit** (`adk-tool`): `stop_server` and
  `shutdown` return once each server's process has exited or the grace period has elapsed,
  instead of right after cancelling its MCP session.
