- **SQLite checkpointer schema upgrade reports failures** (`adk-graph`):
  `SqliteCheckpointer::new` and `from_pool` ignore only the duplicate-column error
  when adding late columns; any other `ALTER TABLE` failure is returned as
  `GraphError::CheckpointError` instead of continuing with a partly upgraded table.
