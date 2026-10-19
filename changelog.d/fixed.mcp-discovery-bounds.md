- **MCP discovery is bounded** (`adk-tool`): `McpToolset` followed a server's `nextCursor`
  through rmcp's `list_all_*` helpers with no page cap or timeout, so a server that always
  returned a cursor held the agent turn until it was interrupted, and two cursors that
  alternated under a `ttlMs` spun on rmcp's response cache without a request. Listings now
  stop at the `McpDiscoveryLimits` bounds with an `AdkError::Tool`, and
  `McpServerManager` lists its servers concurrently, so one hung server no longer blocks
  the others' tools.
- **One MCP server's duplicate name no longer fails the agent turn** (`adk-tool`): a
  server that published the same tool name twice failed every turn with "Duplicate tool
  name". Discovery keeps the first definition and skips later ones with a warning, and
  also skips names that no model can call: empty, containing control characters, or
  longer than the 64 bytes model adapters truncate to. Logged names are escaped.
- **`McpServerManager` prefixed names are collision-checked** (`adk-tool`): server IDs and
  tool names may both contain `__`, so prefixing server `a`'s tool `b__x` and server
  `a__b`'s tool `x` both produced `a__b__x`, which failed the agent turn. Servers are
  visited in ID order, the first to claim a name keeps it, and a later one, or a prefixed
  name over 64 bytes, is skipped with a warning.
- **ACP rejects MCP servers that share a tool name at session setup** (`adk-acp`): two
  attached servers that both published `read_file` created the session and then failed
  every prompt. Session setup now fails with an error naming both servers and the tool.
- **`PrefixedToolset` forwards a wrapped tool's safety flags** (`adk-tool`): it reported
  `is_read_only()` and `is_concurrency_safe()` as `false` for every wrapped tool, so
  parallel dispatch treated prefixed tools as unsafe. It now forwards both, along with
  `effect()` and `annotations()`.
- **`McpToolset::call_tool_value` applies the tool filter** (`adk-tool`): it called a
  tool that `with_filter` or `with_tools` excluded. It now returns an error without
  contacting the server.
