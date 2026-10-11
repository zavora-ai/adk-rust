- **PostgreSQL spend ledger migrations run under an advisory lock** (`adk-session`):
  processes starting together no longer race on `CREATE TABLE`. The same lock covers the
  new PostgreSQL action ledger and approval store.
