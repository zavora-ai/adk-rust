- **`$ref` inlining is linear in the schema** (`adk-core`, `adk-gemini`, `adk-model`):
  `schema_utils::resolve_refs` expands a definition at most once on each path, so a
  reference to a definition already being inlined becomes `{"type": "object"}`, and stops
  inlining past `MAX_INLINED_REF_NODES` (10 000) copied values. A 200-byte schema whose
  definition referred to itself three times took seconds to normalize and grew
  exponentially; an MCP server could publish one. `SchemaCache` no longer holds its lock
  while it normalizes, so one slow schema no longer stalls every request that shares the
  cache.
