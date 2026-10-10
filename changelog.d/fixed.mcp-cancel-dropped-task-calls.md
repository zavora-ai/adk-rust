- **A dropped MCP call cancels its task** (`adk-tool`): when the server turned a
  `tools/call` into a task and the call future was dropped while polling it, such as by
  the agent's tool timeout, the task kept running until `cancel_pending_tasks` ran. The
  toolset now sends `tasks/cancel` for it when the call is dropped, and keeps tracking
  the task until the server reports it finished.
