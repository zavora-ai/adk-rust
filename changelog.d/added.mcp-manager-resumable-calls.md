- **Park-mode calls on a managed MCP server, in the caller's run** (`adk-tool`):
  `McpServerManager::call_tool_resumable(id, ctx, name, arguments)` calls a tool the way
  `McpToolset::call_tool_resumable` does, ending at the server's first question with
  `McpCallOutcome::Parked`, and `McpServerManager::reissue_parked_call_for(ctx, parked,
  responses)` re-issues a parked call. Both run in the scopes the agent's call `ctx`
  belongs to, as a tool call does: sampling requests the server sends meanwhile count
  against that run's budget and spend ledger, and elicitations belong to the call. The
  kernel's drivers call through them. Spec 3.2 §7.7, §10 T2.
