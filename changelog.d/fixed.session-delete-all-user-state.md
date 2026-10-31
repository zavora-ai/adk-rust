- **`delete_all_sessions` erases the user's `user:` tier on every backend** (`adk-session`):
  the in-memory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j and Firestore backends deleted the
  user's sessions and events but left the `(app, user)` user tier, which a new session of the
  same user read back; nothing else removed it. Each backend now deletes the tier with the
  sessions, also when no session is left. Vertex AI, which fell through to the default error,
  deletes each session `list` returns. A shared contract check runs on every backend's suite.
