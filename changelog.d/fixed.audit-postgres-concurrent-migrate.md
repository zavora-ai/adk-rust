- **Concurrent `PostgresAuditSink::migrate` calls** (`adk-auth`): migration runs in one
  transaction under an advisory lock, so processes that call it at the same time on a
  fresh database all succeed. Two concurrent calls failed with a duplicate key on
  `pg_type_typname_nsp_index`.
