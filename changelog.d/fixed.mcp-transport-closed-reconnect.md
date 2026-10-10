- **A crashed MCP server is reconnected** (`adk-tool`): rmcp reports `Transport closed`
  for requests on a stdio server whose process exited, and no reconnect trigger matched
  it, so every later request on the toolset failed. `Transport closed` now triggers a
  reconnect, `McpToolset::is_closed` returns `true` once the transport closes (it only
  checked the cancellation token, so `McpServerManager` health checks missed exited
  servers), and with a connection factory a closed connection is replaced before the
  next request is sent. A `tools/call` that was in flight when the server exited is
  still not resent unless replay is allowed.
