- **`Runner::close` stops MCP servers** (`adk-tool`): `McpToolset` implements
  `Toolset::close`. It cancels the remote tasks the toolset started and its rmcp session,
  then waits for the transport to close: a stdio server's process has exited (rmcp kills one
  still running 3 seconds after its input closed), and a Streamable HTTP session is ended
  with `DELETE`. The wait is bounded by the new `McpToolset::with_close_timeout` (5 seconds
  by default), and closing again is harmless. A closed toolset, its clones and its tools
  fail later requests with the `tool.mcp.closed` error and do not reconnect through a
  connection factory. `FilteredToolset`, `MergedToolset` and `PrefixedToolset` close their
  inner toolsets. Closing an `McpServerManager` as a toolset leaves its servers running;
  its `stop_server` and `shutdown` now return once each server's process has exited or the
  grace period has elapsed.
