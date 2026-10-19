- **A dropped MCP tool call is cancelled on the server** (`adk-tool`): when a `tools/call`
  future is dropped before its response arrives, such as by the agent's tool timeout,
  `McpToolset` sends `notifications/cancelled` for the request, or `tasks/cancel` when the
  server had turned the call into a task, so the server can stop the work. A task stays
  tracked for `cancel_pending_tasks` until the server reports it finished.
