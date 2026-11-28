- **The MCP server governs every call by default** (`adk-tool`, feature `mcp-server`): the
  default executor is now `GovernedExecutor`, which runs each `tools/call` through
  `adk_core::authorize_tool_call` and the action ledger under the `RunConfig` passed to
  `McpServerBuilder::tool_governance` — its tool policy, kill switch, confirmation handler,
  approval store, ledger and audit trail. A server with no config, or a config without a
  `tool_policy`, refuses every call (`mcp.server.call_refused`); an approval-required call is
  held in the approval store (`mcp.server.call_held`) or, with no approver, refused
  (`mcp.server.no_approver`), since the MCP client never decides a confirmation. Call-ID
  decisions in the config are dropped, because the client chooses the request id.
  `DirectExecutor` is renamed `UngovernedExecutor`, the explicit opt-out under which any
  caller runs any visible tool. Setting both `executor(..)` and `tool_governance(..)` fails
  `build()` with `mcp.server.executor_and_governance`. Migration:

  | Before | After |
  |--------|-------|
  | `McpServer::builder(..).tool(t).build()` (every call ran) | `.tool_governance(RunConfig::builder().tool_policy(Arc::new(DeclarativePolicy::builder().allow("get_quote").build())).build())` |
  | `DirectExecutor::new(name)` | `UngovernedExecutor::new(name)`, or `GovernedExecutor::new(name, config)` inside a custom executor |
  | No freeze on served calls | `RunConfig::builder().governance(control)` in the `tool_governance` config |
