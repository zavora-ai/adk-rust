- **Every session backend validates identifiers on every method** (`adk-session`): SQLite,
  PostgreSQL, Redis, MongoDB, and Neo4j validated only in `get`, and the in-memory and encrypted
  services skipped `list`, `delete_all_sessions`, and `append_event`, so an empty, oversized, or
  NUL-containing app name, user ID, or session ID could create, clear, or append to sessions no
  `get` can read. Each method now rejects one with an `InvalidInput` error before it touches
  storage, as Firestore and Vertex AI already did. `CreateRequest::try_identity()` validates the
  app name and user ID when `session_id` is `None`.
