- **Durability gates on every backend** (`adk-durability-gates`, `adk-session`): the crash
  mid-payment, one-firing-per-schedule, stalled-worker, and kill-switch gates run with
  separate worker, scheduler, and replica processes on SQLite, PostgreSQL, Redis, MongoDB,
  Neo4j, and the Firestore emulator. SQLite runs on every pull request; the server backends
  run in the nightly `durable-stores` job. `FirestoreSessionService::from_db` builds the
  session service on an existing client, such as one connected to the emulator.
