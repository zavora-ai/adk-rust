- **A held `edit_file` approved from a new process runs** (`adk-harness`, `adk-cli`): read
  records persist with the session — `SqliteReadRecords` in `harness.db` (sealed under the
  store key, covered by `adk-rust store status` and `rotate-key`), `PostgresReadRecords`
  and `RedisReadRecords` for `serve --store` — through `HarnessAgentBuilder::read_records`,
  so a goal parked on a held edit no longer fails with `devtools.not_read` after a restart
  (spec 3.1 §7.33 fix 5).
