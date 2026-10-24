- **Cron watermarks** (`adk-kernel`; spec 3.2 §7.4, §7.13): `CronWatermarkStore` keeps where
  each cron wake source left off, on in-memory, SQLite, PostgreSQL and Redis
  (`InMemoryCronWatermarkStore`, `SqliteCronWatermarkStore`, `PostgresCronWatermarkStore`,
  `RedisCronWatermarkStore`), with `cron_watermark_store_conformance_tests!`; a cursor only
  moves forward. `CronWakeSource::watermarked` plans the ticks missed while no kernel watched
  with the wake's `MissedFirePolicy` — `skip` the latest only when on time, `fireOnce` the
  latest, `catchUp(n)` the latest `n` — writing the cursor before each tick goes out, so each
  missed tick fires once across restarts and replicas. A tenant kernel's cron sources keep
  their cursors in the tenant's own store (`TenantKernelBuilder::watermark_store`), and
  `ManagedKernels` refuses a tenant whose watermark store another tenant holds. A missed tick
  carries `catch_up_rank` and `on_time`, so a cron wake fires only for the missed ticks its own
  policy picks when wake sets with other policies share an expression.
