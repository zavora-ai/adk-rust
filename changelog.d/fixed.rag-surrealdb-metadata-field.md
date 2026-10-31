- **SurrealDB collections define their `metadata` field** (`adk-rag`): `create_collection` declared
  `metadata` as `object FLEXIBLE`, which SurrealDB 3 refuses on a schemaless table, and the error
  went unread, so the field stayed undefined. It is now `object`, and `create_collection` returns
  any statement's error.
