- **Cron sleepers keep their cursor across restarts** (`adk-kernel`, `adk-cli`; spec 3.2 §7.4):
  the kernel kept a sleeper's cron cursor in memory, so a restarted kernel or another replica
  fired nothing that came due while no kernel ran. The cursor is now kept in the kernel's
  `CronWatermarkStore` under `kernel:<expression>` (`KernelBuilder::watermark_store`, in memory
  by default; `adk-rust up` opens it on `harness.db` or the `--store` backend). The occurrences
  missed while no kernel ran fire once each, one at a time, as each sleeper's `missed` policy
  picks them, and never one that already fired; a new series replays nothing from before it
  began. A tenant kernel deletes an expression's cursor when a definition hosted again leaves no
  wake set on it, so a wake set that uses the expression later replays nothing from the idle
  time.
