- **An in-memory `GraphMemoryService` keeps its tables** (`adk-memory`): on
  `sqlite::memory:` the pool closed its connection after 10 minutes idle, which dropped the
  database, and the next call failed with `no such table: kg_entities`. An in-memory URL now
  gets a pool of one connection with no idle timeout or maximum lifetime. File-backed
  databases keep the pool's defaults.
