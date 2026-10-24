- **`harness.db` runs in SQLite's write-ahead-log mode** (`adk-cli`): readers no longer
  block the writer, and a commit appends to `harness.db-wal` instead of rewriting a rollback
  journal. On a Windows runner a rollback-journal commit took 127 ms against 29 ms in WAL,
  so with `adk-rust up`, `chat` and `serve` sharing the file a writer could wait past the
  30-second busy timeout and fail with `database is locked`. The mode is stored in the file;
  while an older process still holds it in rollback mode, a newer one keeps that mode and
  logs a warning. `harness.db-wal` and `harness.db-shm` sit beside the database while it is
  open and get its owner-only protection: on Unix its mode, and on Windows the directory
  holding `harness.db` gets a protected DACL granting its owner alone, inherited by every
  file created in it.
