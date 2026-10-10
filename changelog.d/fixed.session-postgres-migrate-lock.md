- **PostgreSQL `migrate()` releases its advisory lock** (`adk-session`, `adk-memory`):
  `PostgresSessionService::migrate` and `PostgresMemoryService::migrate` take the
  advisory lock, run every migration statement, and unlock on one connection, then close
  that connection instead of returning it to the pool. The unlock ran on whichever pooled
  connection was free, so an idle pooled connection kept the lock and a second instance
  calling `migrate()` blocked until the first process exited. The new
  `migration::{pg_runner, sqlite_runner}::run_sql_migrations_on_connection` runs a
  migration on a single connection.
