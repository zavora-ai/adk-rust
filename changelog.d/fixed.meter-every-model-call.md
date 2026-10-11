- **Compaction, routing and MCP sampling calls are metered** (`adk-runner`, `adk-agent`,
  `adk-tool`): post-run, intra-invocation and token-limit compaction summaries,
  `LlmConditionalAgent` routing and `LlmSamplingHandler` requests now check
  `RunConfig::budget_tracker` before the call is sent, count its tokens and cost, and
  reserve, commit and release in `RunConfig::spend_ledger` under the run's app, agent and
  reported provider. A call either gate refuses is never sent; a refused compaction is
  skipped with a warning.
