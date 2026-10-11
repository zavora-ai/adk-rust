- **`RunnerConfig` gains `action_ledger`, `approval_store`, and `spend_ledger`**
  (`adk-runner`): a `RunnerConfig` struct literal must set the three fields, to `None` for
  the previous behaviour.
- **SQL migration steps are `'static`** (`adk-session`): `migration::sqlite_runner` and
  `migration::pg_runner` take `&'static [(i64, &'static str, &'static str)]`, so a store's
  `migrate()` future is `Send` and can run inside a spawned task or an `async_trait`
  method.
