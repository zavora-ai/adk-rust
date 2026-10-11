- **Durable stores on every session backend** (`adk-session`): `PostgresActionLedger`,
  `SqliteApprovalStore`, and `PostgresApprovalStore`, plus a spend ledger, action ledger,
  and approval store for Redis (`RedisSpendLedger`, `RedisActionLedger`,
  `RedisApprovalStore`), MongoDB (`Mongo*`), Neo4j (`Neo4j*`), and Firestore
  (`Firestore*`), each behind its backend's existing feature. Every type has `new`, a
  constructor over the session service's handle, and `migrate()`, and passes the shared
  conformance suites.
- **Durable stores on the runner builder** (`adk-runner`): `Runner::builder()` takes
  `action_ledger`, `approval_store`, and `spend_ledger`, installed on every run whose
  `RunConfig` leaves them unset.
