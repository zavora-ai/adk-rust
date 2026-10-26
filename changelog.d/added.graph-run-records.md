- **Run and step records, and the graph's shape** (`adk-graph`): every checkpoint carries
  its run (`adk.graph.run`), the steps finished since the previous one with each node's
  start, duration, status, written keys and attempt (`adk.graph.steps`), and a fork's
  lineage (`adk.graph.lineage`), read with `Checkpoint::run_record`, `step_records` and
  `lineage`. `CompiledGraph::shape` reports nodes, edges and entry nodes in a
  deterministic order, and the provided `Node::kind` labels each node (spec 3.3 §12).
