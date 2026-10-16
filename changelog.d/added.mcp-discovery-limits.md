- **`McpDiscoveryLimits`** (`adk-tool`): bounds each MCP catalog listing (`tools/list`,
  `resources/list`, `resources/templates/list`, `prompts/list`) to `max_pages` pages
  (default 64) and an overall `timeout` including reconnects (default 30 seconds), and
  fails a listing whose server repeats a cursor. Set it with `with_discovery_limits` on
  `McpToolset`, `McpServerManager`, `ConnectionRefresher`, and `SimpleClient`.
- **`Tool::annotations`** (`adk-core`): returns the behaviour annotations a tool's
  publisher declared. MCP tools return the server's `annotations` object, so policies
  and callbacks can read `title`, `destructiveHint`, and `openWorldHint`.
  `PrefixedToolset` and `McpServerManager`'s prefixed tools forward it.
