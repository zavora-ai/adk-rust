- **`McpDiscoveryLimits`** (`adk-tool`): bounds each MCP catalog listing (`tools/list`,
  `resources/list`, `resources/templates/list`, `prompts/list`) to `max_pages` pages
  (default 64) and an overall `timeout` including reconnects (default 30 seconds), and
  fails a listing whose server repeats a cursor. Set it with `with_discovery_limits` on
  `McpToolset`, `McpServerManager`, `ConnectionRefresher`, and `SimpleClient`.
- **`Tool::annotations`** (`adk-core`, `adk-tool`): returns the behaviour annotations a
  tool's publisher declared, `None` by default. MCP tools return the server's `annotations`
  object, so policies and callbacks can read `title`, `destructiveHint`, and
  `openWorldHint`, and their `effect()` follows it: `readOnlyHint` gives
  `ToolEffect::ReadOnly`, `idempotentHint` gives `ToolEffect::Idempotent`, and anything else
  `ToolEffect::NonIdempotent`. `PrefixedToolset` and `McpServerManager`'s prefixed tools
  forward it.
