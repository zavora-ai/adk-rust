- **Graph checkpoints carry a per-thread version** (`adk-graph`): `Checkpoint` gains
  `version` (from 1) and `pending_writes`, and derives `PartialEq`. `Checkpointer::save`
  refuses a version that is not greater than every stored version of the thread with
  `GraphError::CheckpointConflict` (`graph.checkpoint.conflict`), and refuses a stored
  checkpoint ID. `load` and `list` order by version instead of `created_at`. A caller
  that saves several checkpoints on one thread sets `Checkpoint::with_version(newest + 1)`.
  `Checkpointer::prune` no longer has a default and `list_page` is added with one.
  `ExecutionConfig` gains a public `durability` field. `SqliteCheckpointer` numbers rows of
  an existing database in creation order on first open.
