- **`GraphError` variants** (`adk-graph`): `GraphError` gains `AgentFailed`, `FanInIncomplete`,
  `CheckpointConflict`, `Suspended`, and `Cancelled`, so an exhaustive `match` needs their arms.
  `GraphError::DatabaseError` and its `From<sqlx::Error>` conversion are removed: the variant
  existed only with the `sqlite` feature, so enabling the feature changed the enum, and the crate
  never constructed it. Checkpointer database failures remain `GraphError::CheckpointError`.
