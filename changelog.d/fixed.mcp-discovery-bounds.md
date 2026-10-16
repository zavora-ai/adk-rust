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
- **`McpServerManager` prefixed names are collision-checked** (`adk-tool`): server
  `a` with tool `b__x` and server `a__b` with tool `x` both resolved to `a__b__x`, which
  failed the agent turn. The first server in ID order keeps the name, a later one is
  skipped with a warning, and a prefixed name over 64 bytes is skipped.
- **ACP rejects MCP servers that share a tool name at session setup** (`adk-acp`): two
  attached servers that both published `read_file` created the session and then failed
  every prompt. Session setup now fails with an error naming both servers and the tool.
- **Tool-list invalidation is not lost mid-fetch** (`adk-tool`):
  `McpToolset::invalidate_tool_list_cache` during an in-flight `tools/list` let that
  fetch cache the stale list for up to 60 seconds, and a page the server marked fresh
  with `ttlMs` survived invalidation in rmcp's response cache. A fetch now caches its
  result only when no invalidation happened since it began, and invalidation clears
  rmcp's response cache.
- **`idempotentHint` makes an MCP tool `ToolEffect::Idempotent`** (`adk-tool`): an MCP
  tool annotated only `idempotentHint: true` reported `NonIdempotent`, so it was never
  retried and was recorded in the action ledger. `PrefixedToolset` now also forwards
  `is_read_only` and `is_concurrency_safe`, which it reported as `false` for every
  wrapped tool.
- **`McpToolset::call_tool_value` applies the tool filter** (`adk-tool`): it called a
  tool that `with_filter` or `with_tools` excluded. It now returns an error without
  contacting the server.
