- **Session rewind takes the full session identity** (`adk-session`):
  `SessionService::rewind` and `rewind_steps` take `&AdkIdentity` instead of a bare session
  ID, and match app, user, and session, so one user's rewind no longer truncates another
  user's session with the same ID. Rewind rebuilds the session state from the state the
  session was created with plus the remaining events' deltas, instead of from the deltas
  alone. Errors are typed: `session.rewind.unsupported` (`Unsupported`, the default for
  backends without rewind), `session.not_found`, `session.event_not_found`, and
  `session.rewind.too_many_steps` (`InvalidInput`). `EncryptedSession` now forwards rewind,
  counting `rewind_steps` over the events callers see. SQLite migration 2 adds
  `sessions.initial_state`; a session created before it rewinds to its keys that no event has
  written.
- **`create` for an existing session fails with `session.already_exists`** (`adk-session`):
  every backend returns a `Conflict` error and writes nothing when the `(app_name, user_id,
  session_id)` identity exists. The in-memory and Redis backends previously replaced the
  session, dropping its events (in-memory) or its state (Redis); SQLite, PostgreSQL, MongoDB,
  and Neo4j returned an untyped internal error; Firestore's error moves from `InvalidInput` to
  `Conflict`.
