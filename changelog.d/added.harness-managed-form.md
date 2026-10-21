- **The harness on the managed runtime** (`adk-harness`, `adk-managed`): `packs::managed::HarnessDef`
  (feature `managed`) declares a harness as a `ManagedAgentDef` — mode, shell policy, skills roots,
  members and packs, carried in the definition's `adk.harness` metadata — and
  `HarnessResolver` builds it inside `DefaultManagedAgentRuntime` through the new
  `AgentResolver` hook (`with_agent_resolver`). Held confirmations and custom tool calls park on
  the runtime's durable runs; before each turn the harness answers a call that ran without its
  result saved from the action ledger, and holds again a call whose decision was lost before it
  began. A definition cannot run a host shell, load the operator's `AGENTS.md` or home skills,
  or name skills outside its workspace unless the operator allows it.
  `HarnessResolver::with_harness` serves an application's own `HarnessAgent` for a definition
  named like it, so one harness runs behind `Launcher`, `ServerBuilder`, `serve_agent_engine`
  and the managed runtime (spec 3.1 §7.18, H9). `HarnessAgentBuilder::pack(Pack::Managed)`
  marks a harness for the managed runtime instead of failing as `not yet implemented`.
  Experimental, as `adk-managed` is.
