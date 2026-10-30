- **SQLite fact changes wait for other writers** (`adk-harness`): `SqliteFactStore` opens each
  change that reads a fact and writes it back with `BEGIN IMMEDIATE`. In `harness.db`, where
  `adk-rust up`'s kernel writes beside the facts, a consolidation tick, a recall or a correction
  no longer fails at once with `database is locked`; it waits under the pool's busy timeout.
