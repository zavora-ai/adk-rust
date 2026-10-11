- **Graph checkpointers for shared databases** (`adk-graph`): `PostgresCheckpointer`,
  `RedisCheckpointer`, `MongoCheckpointer`, `Neo4jCheckpointer`, and
  `FirestoreCheckpointer` behind the `postgres`, `redis`, `mongodb`, `neo4j`, and
  `firestore` features, with `new`, `from_*`, and `migrate()`. Forwarded by `adk-rust`
  as `graph-postgres`, `graph-redis`, `graph-mongodb`, `graph-neo4j`, and
  `graph-firestore`.
