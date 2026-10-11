- **Durable runtime stores on Redis, MongoDB, Neo4j, and Firestore** (`adk-session`):
  `LeaseStore`, `DurableRunStore`, `ScheduleStore`, and `GovernanceStateStore` for each
  backend (`Redis*`, `Mongo*`, `Neo4j*`, `Firestore*` — `LeaseStore`, `RunStore`,
  `ScheduleStore`, `GovernanceStateStore`), behind the backend's existing feature. Every type
  has `new`, a constructor over a shared handle, and `migrate()`, and passes the `adk-core`
  conformance suites. Redis uses Lua scripts with no key expiry; MongoDB uses single-document
  conditional writes and runs on a standalone `mongod`; Neo4j and Firestore use transactions.
  The `neo4j` feature now enables `tokio` for transaction retry backoff.
