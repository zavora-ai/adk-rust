- **The partition store on MongoDB, Firestore and Neo4j (Alpha)** (`adk-kernel`; spec 3.2
  §7.8, §13 Q8): `MongoPartitionStore`, `FirestorePartitionStore` and `Neo4jPartitionStore`
  behind the `mongodb`, `firestore` and `neo4j` features, taking the client handle the
  kernel's other stores on that backend take. `partition_store_conformance_tests!` runs on
  each as part of the `#[ignore]`d live suites of the nightly `durable-stores` job, and the
  backends stay Alpha until those suites pass there.
