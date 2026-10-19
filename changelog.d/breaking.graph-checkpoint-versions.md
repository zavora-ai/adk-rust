- **Graph checkpoints carry a per-thread version** (`adk-graph`): `Checkpoint` gains the public
  fields `version` (from 1) and `pending_writes`, so a struct literal must set both
  (`Checkpoint::new` does), and derives `PartialEq`. `Checkpointer::save` refuses a version that
  is not greater than every stored version of the thread with `GraphError::CheckpointConflict`
  (`graph.checkpoint.conflict`), and refuses version 0 and a stored checkpoint ID with
  `GraphError::CheckpointError`. `load` and `list` order by version instead of creation time. A
  caller that saves several checkpoints on one thread sets `Checkpoint::with_version(newest + 1)`.
- **`Checkpointer::prune` is required** (`adk-graph`): a custom `Checkpointer` implements
  `prune`, which no longer has a default that kept everything, and keeps the contract above.
  `list_page` is added with a default built on `list`. `SqliteCheckpointer` numbers the rows of
  an existing database in creation order when it opens it.
