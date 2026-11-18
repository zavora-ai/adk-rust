- **YAML and JSON graph workflows** (`adk-graph`, feature `yaml`; `adk-rust` feature
  `graph-yaml`, in `full`; `adk-cli`): `WorkflowDefinition` reads a workflow file and compiles
  it into a graph against a `WorkflowRegistry`. The file declares `agent` and `llm` nodes,
  `function`, `set`, `action` and `interrupt` nodes, chains, fan-out, conditional `routes` and
  `switch` edges, state reducers, `interrupt_before`/`interrupt_after`, the checkpoint store and
  the recursion limit. Conditions are data (`field`, `op`, `value`, with `all`, `any`, `not`)
  over `adk-action`'s switch operators. Every parse, validation and registry issue reports its
  path, and for YAML its line and column. `schemas/workflow.schema.json` gives editors
  completion. `adk-rust graph run`, `graph validate` and `graph schema` run and check a file;
  a pause asks for its answer on stdin or resumes with `--resume`. `AgentNode::with_name`
  names an agent node apart from its agent.
