- **Checkpoint retention keeps what a resume needs** (`adk-graph`, `adk-harness`):
  `RetentionPolicy` no longer removes a pause no run has resumed, whose ID a caller holds
  from `GraphError::Interrupted`, or an effect-log entry, on any backend. Every checkpoint
  a run writes lists the thread's open pauses (`Checkpoint::open_pauses`), functional
  workflows included, and `SealedCheckpointer` pins them from its decrypted view.
- **A removed resume point is refused** (`adk-graph`): `ExecutionConfig::with_resume_from`
  naming a checkpoint that retention or `Checkpointer::delete` removed fails with
  `GraphError::CheckpointError` before any node runs, instead of starting the run again from
  its entry.
