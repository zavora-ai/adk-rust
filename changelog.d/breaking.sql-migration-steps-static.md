- **SQL migration steps are `'static`** (`adk-session`):
  `migration::sqlite_runner::run_sql_migrations` and `migration::pg_runner::run_sql_migrations`
  take `steps: &'static [(i64, &'static str, &'static str)]`, so a store's `migrate()` future
  is `Send` and can run inside a spawned task or an `async_trait` method.
