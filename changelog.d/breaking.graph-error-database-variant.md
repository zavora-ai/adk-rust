- **`GraphError::DatabaseError` removed** (`adk-graph`): the variant existed only
  with the `sqlite` feature, so enabling the feature changed the enum, and it was
  never constructed. Checkpointer database failures remain
  `GraphError::CheckpointError`. Remove match arms on `DatabaseError`.
