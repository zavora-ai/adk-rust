- **Durability modes and pending writes** (`adk-graph`):
  `ExecutionConfig::with_durability(Durability::{Sync, Async, Exit})` sets when each
  super-step is checkpointed (default `Sync`); `CompiledGraph::with_node_effect` makes a
  super-step with a `NonIdempotent` node write earlier checkpoints first. When one node
  of a parallel super-step fails, the finished nodes' outputs are saved as
  `Checkpoint::pending_writes` and a resume does not run them again.
