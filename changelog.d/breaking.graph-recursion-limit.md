- **The graph's recursion limit bounds its runs** (`adk-graph`):
  `CompiledGraph::with_recursion_limit` and `GraphAgentBuilder::recursion_limit` had no
  effect, because the executor read only `ExecutionConfig::recursion_limit`, whose default of
  50 capped every run, so a `GraphAgent` turn could never take more than 50 super-steps.
  `ExecutionConfig::recursion_limit` is now `Option<usize>`: `None` (the default) uses the
  graph's limit, 100 unless set, and `with_recursion_limit` overrides it for one run. Code
  that read the field as a `usize` reads an `Option`.
  `adk ultracode` sizes its graph's limit from `--max-rounds`, since its cap of 16 now applies.
