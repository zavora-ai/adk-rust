- **Fork, rewind, and edit session history on every backend** (`adk-session`):
  `SessionService::fork` creates a session of the same app and user holding the source's events
  up to an event, with the state there, and records its origin under `adk:forked_from`;
  `rewrite_history` rewinds or edits events in place (`HistoryEdit::Replace`,
  `HistoryEdit::Remove`) and returns a `HistoryChange` for the audit log; `history` reads the
  events, state, creation state, and a version for optimistic concurrency; `import_session`
  creates a session that already holds events. In-memory, SQLite, PostgreSQL, Redis (standalone
  and cluster), MongoDB, Neo4j, Firestore, `EncryptedSession`, and `ConsentGatedSessionService`
  support every operation atomically per session; Vertex AI forks and reports in-place rewrites
  as `session.history.unsupported`. One conformance suite runs on every backend.
