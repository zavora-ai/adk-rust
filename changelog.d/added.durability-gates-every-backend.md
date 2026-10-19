- **Durable runtime verified on every backend** (`adk-session`, `adk-server`): the
  multi-process durability gates (crash mid-payment, one firing per schedule, stalled worker,
  and kill switch, with separate worker, scheduler, and replica processes) run on SQLite,
  PostgreSQL, Redis, MongoDB, Neo4j, and the Firestore emulator. SQLite runs on every pull
  request; the server backends run in the nightly `durable-stores` job.
- **`FirestoreSessionService::from_db`** (`adk-session`): builds the session service on an
  existing `FirestoreDb`, such as one from `firestore_stores::connect_firestore_emulator`.
