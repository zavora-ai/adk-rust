- **`ADVISORY_LOCK_KEY` is public** (`adk-session`, `adk-memory`):
  `PostgresSessionService::ADVISORY_LOCK_KEY` and `PostgresMemoryService::ADVISORY_LOCK_KEY`
  name the `pg_advisory_lock` key that `migrate()` holds, so operators can find it in
  `pg_locks`.
