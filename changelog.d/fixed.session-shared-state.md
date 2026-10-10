- **Session backends serve current `app:` and `user:` state** (`adk-session`): `get` and
  `list` on the SQLite, PostgreSQL, Redis, MongoDB, Firestore, and Neo4j backends read
  the app and user tiers at call time instead of returning the copy stored on the
  session at its last write, so a value written through one session reaches the other
  sessions of the app or user. The in-memory backend's `list` does the same. Session
  records now store only session-scoped keys; tier copies in records written by earlier
  releases are ignored on read.
- **Concurrent appends keep every state delta** (`adk-session`): PostgreSQL merges deltas
  in SQL (`state || delta`), SQLite reads and writes state inside `BEGIN IMMEDIATE`
  transactions, MongoDB merges with an `$mergeObjects` update pipeline, and Firestore
  merges inside its commit transaction. Redis writes only the delta's fields to the app
  and user hashes and replaces the session state through a compare-and-set Lua script,
  so the server must allow `EVAL`. Concurrent appends previously overwrote each other's
  keys, and on SQLite failed with `database is locked`. A `null` delta value is stored as
  `null` on every backend.
- **PostgreSQL baseline detection is limited to the current schema** (`adk-session`,
  `adk-memory`): `migrate()` counts a `sessions` or `memory_entries` table only in
  `current_schema()`. A table of that name in another schema of the same database was
  recorded as the baseline, so the tables were never created in the current schema.
