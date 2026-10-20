- **`telemetry-sqlite` combines with the SQLite backends** (`adk-telemetry`): the span
  exporter depended on rusqlite 0.32, which needs `libsqlite3-sys` 0.30, while SQLx 0.9
  resolves to `libsqlite3-sys` 0.37 in a new lockfile. Only one crate may link `sqlite3`, so
  adding `telemetry-sqlite` to a project that already used `SqliteSessionService`,
  `SqliteMemoryService`, or `graph-sqlite` failed to resolve until `libsqlite3-sys` was
  downgraded by hand. The exporter now uses rusqlite 0.39, which links `libsqlite3-sys` 0.37,
  the newest release SQLx 0.9 accepts.
