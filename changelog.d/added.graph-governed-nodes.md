- **Governed graph nodes** (`adk-graph`): `CompiledGraph::with_governed_node` and
  `GraphAgentBuilder::governed_node` put a node's call through the kill switch, the
  action ledger and `authorize_tool_call`, so it waits for a decision and runs at most
  once per call in its lineage, forks included; an unchanged call in a fork is answered
  from the effect log. The provided `Node::call_args` and `Node::run_call` describe the
  call and run an approver's edit; HTTP action nodes implement both, with credentials
  shown as `sha256:` digests, keyed with `HttpActionPolicy::credential_key` when the host
  sets one. `Node::will_act` keeps a node that does nothing, or cannot act, from asking
  anyone; a governed action node's call is sent once, whatever its error handling; a run
  without an action ledger or a checkpointer does not run a governed node; and
  `CompiledGraph::governed_shapes` lists governed nodes inside subgraphs too. A graph that
  governs no node behaves as before (spec 3.3 §12).
