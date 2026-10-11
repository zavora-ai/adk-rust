- **Checkpointer conformance suite** (`adk-graph`): the `conformance` feature exposes
  `CheckpointerHarness` and `checkpointer_conformance_tests!`, covering round trip,
  version ordering, paging, delete, prune policies, thread isolation, concurrent
  writers, restart survival, large state, and identifier escaping. Memory and SQLite
  run it on every PR; PostgreSQL, Redis, MongoDB, Neo4j, and the Firestore emulator
  run it in the nightly `durable-stores` job.
