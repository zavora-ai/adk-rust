- **`McpToolset` caches `tools/list`** (`adk-tool`): discovery reuses a server's tool
  list for 60 seconds instead of sending `tools/list` before every model turn. The cache
  is dropped on reconnect, on `notifications/tools/list_changed` for connections served
  by `AdkClientHandler`, and by the new `invalidate_tool_list_cache()`. Set the TTL with
  the new `with_tool_list_cache_ttl`; `Duration::ZERO` restores a request per
  resolution.
