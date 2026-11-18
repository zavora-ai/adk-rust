- **`MemoryEntry` carries an id and an expiry** (`adk-memory`): the struct gains `id` and
  `expires_at` and is `#[non_exhaustive]`; build it with `MemoryEntry::new` and its `with_*`
  methods. `MemoryService` gains `delete_memory` and `purge_expired`, which a wrapper forwards.
  `RedisMemoryConfig::ttl` is the default TTL of each entry rather than a whole session list's.
  `migrate()` adds the columns and indexes to SQLite, PostgreSQL, MongoDB, Neo4j and the
  knowledge graph, giving existing entries ids.
