- **A dropped MCP tool call is cancelled on the server** (`adk-tool`): when a
  `tools/call` future is dropped before its response arrives, such as by the agent's
  tool timeout, the toolset sends `notifications/cancelled` for the request so the
  server can stop the work.
