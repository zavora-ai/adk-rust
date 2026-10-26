- **Governed graph nodes** (`adk-graph`): `CompiledGraph::with_governed_node` and
  `GraphAgentBuilder::governed_node` put a node's call through the kill switch, the
  action ledger and `authorize_tool_call`, so it waits for a decision and runs at most
  once per call in its lineage, forks included; an unchanged call in a fork is answered
  from the effect log. The provided `Node::call_args` and `Node::run_call` describe the
  call and run an approver's edit; HTTP action nodes implement both, with credentials
  shown as `sha256:` digests. A graph that governs no node behaves as before
  (spec 3.3 §12).
