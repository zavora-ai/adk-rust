- **`ToolExecutionStrategy::Auto` keeps the model's call order** (`adk-agent`): only a run of
  adjacent read-only, concurrency-safe calls overlaps, and any other call runs alone in its place.
  Before, `Auto` ran every safe call first, so a read listed after a write in the same step ran
  before the write and returned stale data.
