- **The kernel's stores on MongoDB, Firestore and Neo4j (Alpha)** (`adk-kernel`, `adk-rust`;
  spec 3.2 §7.1, §13 Q8).
  - Features `mongodb`, `firestore` and `neo4j` on `adk-kernel`: `ProcessStore`,
    `AgendaStore`, `DriverStore` and `LedgerStore` on each backend
    (`MongoProcessStore`, `MongoAgendaStore`, `stores::MongoDriverStore`,
    `proc::MongoLedgerStore`, and the `Firestore*` and `Neo4j*` stores beside them). Each
    feature also turns on the same backend of `adk-session`'s run, schedule and governance
    stores, and every store takes the client handle those take (`from_database`,
    `from_db`, `from_graph`).
  - Labelled **Alpha** in `STABILITY.md`, the crate README and the kernel docs until each
    store's conformance suite passes against a live server in CI. The suites are `#[ignore]`d
    tests (`tests/store_conformance_{mongodb,firestore,neo4j}.rs`) that the nightly
    `durable-stores` job runs against a MongoDB replica set and standalone server, Neo4j 5
    and the Firestore emulator; `gh workflow run ci-nightly.yml -f job=durable-stores` runs
    that job alone.
  - The umbrella's `mongodb-session`, `firestore-session` and `neo4j-session` features turn
    on the same backend of `adk-kernel` when `harness-kernel` is enabled.
  - New page: [Kernel store backends](docs/official_docs/kernel/backends.md).
