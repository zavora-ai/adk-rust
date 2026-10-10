- **`#[entrypoint]` resumes from the checkpoint `resume_from` names** (`adk-graph`,
  `adk-rust-macros`): the generated `invoke` loads `ExecutionConfig::resume_from` by
  checkpoint id instead of loading the thread's latest checkpoint whatever value it
  held, and returns `GraphError::CheckpointError` without running a task when the
  thread has no checkpoint with that id, including another thread's. To resume where
  a thread left off, pass the id `Checkpointer::load(thread_id)` returns.
  `ExecutionConfig` gains a public `resume_values` field, so a struct literal must set
  it; `ExecutionConfig::new` callers are unaffected.
