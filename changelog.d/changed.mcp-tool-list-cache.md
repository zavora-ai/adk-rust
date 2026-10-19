- **`McpToolset` caches `tools/list`** (`adk-tool`): discovery reuses a server's tool
  list for 60 seconds instead of sending `tools/list` before every model turn. The cache
  is dropped on reconnect, on `notifications/tools/list_changed` for connections served
  by `AdkClientHandler`, and by the new `invalidate_tool_list_cache()`, which also clears
  rmcp's response cache and keeps a `tools/list` already in flight from caching its result.
  Set the TTL with the new `with_tool_list_cache_ttl`; `Duration::ZERO` sends `tools/list`
  on every resolution again, except for pages the server marked fresh with `ttlMs`, which
  rmcp's response cache answers.
