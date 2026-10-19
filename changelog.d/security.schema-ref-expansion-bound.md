- **`$ref` inlining is bounded** (`adk-core`, `adk-gemini`): `schema_utils::resolve_refs`
  inlines a definition at most once on each path, so a reference to a definition already
  being inlined becomes `{"type": "object"}` plus its sibling keywords, and it stops inlining
  once `MAX_INLINED_REF_NODES` (10 000) values have been copied. A recursive definition was
  expanded ten levels deep, so a 200-byte schema whose definition referred to itself three
  times took seconds to normalize and grew exponentially; an MCP server could publish one.
  `SchemaCache::normalize` no longer holds the cache lock while it normalizes, so one slow
  schema no longer stalls every request that shares the cache.
