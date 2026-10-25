- **Agents a person adds, kept by the kernel** (`adk-kernel`): `Kernel::agents()` is the audited
  agent table of spec 3.3 §5.7. A person adds an agent by hand — `preview`, then `apply` with
  the preview's digest — or approves one the primary agent suggested with `propose`, a
  `kernel:agent.create` inbox request the next tick applies only on a person's approval of the
  same definition, within the suggesting agent's limits then. `AgentRecord`, `AgentDefinition`
  and `AgentStore` run on in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j
  with `agent_store_conformance_tests!`. An `AgentFactory` (`KernelBuilder::agent_factory`)
  builds each active agent into a live target table every worker reads, with its progress
  source attached; the kernel refuses a runner under another kill switch. `pause`, `resume`
  and `remove` (which ends its work, routines, waiting decisions and follow-ups and keeps a
  tombstone, so an id is never given again) record `kernel.agent.*` before they act.
  `GET /api/kernel/agents[/{id}]` and `ControlClient::{agents, agent}` read the table;
  `KernelEvent::Agent`, `ProcessFilter::target` and `Suspension::Paused` are new. An agent's
  own daily limit holds its unattended work until the next UTC day, and a chat card for a
  suggested agent offers "Don't add" only. See `docs/official_docs/kernel/agents.md`.
