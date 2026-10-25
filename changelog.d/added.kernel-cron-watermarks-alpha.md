- **The cron watermark store on MongoDB, Firestore and Neo4j (Alpha)** (`adk-kernel`; spec
  3.2 §7.4, §13 Q8): `MongoCronWatermarkStore`, `FirestoreCronWatermarkStore` and
  `Neo4jCronWatermarkStore` behind the `mongodb`, `firestore` and `neo4j` features, taking the
  client handle the kernel's other stores on that backend take. An advance never moves a
  cursor back: a `findOneAndUpdate` upsert with `$max` on MongoDB, a read and write in one
  transaction on Firestore, and one `MERGE` whose `SET` keeps the later cursor on Neo4j.
  `cron_watermark_store_conformance_tests!` runs on each as part of the `#[ignore]`d live
  suites of the nightly `durable-stores` job, and the backends stay Alpha until those suites
  pass there.
