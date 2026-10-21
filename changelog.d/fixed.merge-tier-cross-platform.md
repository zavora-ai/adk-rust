- **SQLite session writers wait up to 30 seconds for the database lock** (`adk-session`):
  `SqliteSessionService::new` sets the connection's busy timeout, so concurrent appends to a
  file database queue behind `BEGIN IMMEDIATE` instead of failing with `database is locked`
  on a slow disk.
- **The workspace test suite passes on macOS and Windows**: path assertions in `adk-devtools`,
  `adk-harness` and `adk-plugin` tests no longer assume `/` or a Unix home directory, the
  plugin invocation-context test counts shared calls atomically, and the long-term memory
  property accepts a superseded known fact that keeps its verified quote.
