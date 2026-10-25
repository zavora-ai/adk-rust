- **A live target table for durable workers** (`adk-server`, feature `background`):
  `AgentTable` is a shared map of agent target → `Runner` with a set of expected targets
  (`insert`, `remove`, `get`, `expect`, `forget`), and `DurableWorker::with_agent_table`
  reads it on every claim after the targets registered with `with_agent`. A target
  inserted while the worker runs serves the next claim on every queue, a removed one is
  refused, and a claim of a target the table expects but has not built yet is handed back,
  claimable one poll interval later, instead of failing. `with_agent` keeps its behaviour
  (spec 3.3 §10 S4).
