- **Durable runtime stores for SQLite and PostgreSQL** (`adk-session`, `sqlite` / `postgres`):
  `SqliteLeaseStore`, `SqliteRunStore`, `SqliteScheduleStore`, `SqliteGovernanceStateStore`
  and their `Postgres*` counterparts. Each has `new(url)`, `from_pool`, and versioned
  `migrate()` (advisory-locked on PostgreSQL), and passes the `adk-core` lease, run,
  schedule, and governance conformance suites. PostgreSQL claims runs with
  `SELECT ... FOR UPDATE SKIP LOCKED`, SQLite under `BEGIN IMMEDIATE`; every worker write is
  fenced on the run's lease epoch.
