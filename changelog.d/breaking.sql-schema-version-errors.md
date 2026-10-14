- **`sql_schema_version` reports read failures** (`adk-session`, `adk-memory`): the
  migration runners' `sql_schema_version` and the SQL services' `schema_version()`
  return 0 only when the registry table does not exist. A lost connection, a missing
  privilege or a misshapen registry is returned as an error instead of version 0.
