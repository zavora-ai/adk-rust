- **The kernel's MongoDB, Firestore and Neo4j stores are Beta** (`adk-kernel`; spec 3.2 §13
  Q8): `ProcessStore`, `AgendaStore`, `DriverStore`, `LedgerStore`, `PartitionStore` and
  `CronWatermarkStore` on MongoDB, Firestore and Neo4j are no longer labelled Alpha and carry
  the crate's Beta contract in `STABILITY.md`, which covers their storage layout. Each store's
  conformance suite passed against live servers in the nightly `durable-stores` job. The API
  and behaviour are unchanged.
