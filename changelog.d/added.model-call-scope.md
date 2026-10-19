- **`ModelCallScope`** (`adk-core`): applies a run's budget and spend ledger to every model
  call made on behalf of the run — `LlmAgent` and `CodeActAgent` turns, post-run,
  intra-invocation, and token-limit compaction summaries, `LlmConditionalAgent` routing, and
  `LlmSamplingHandler` requests. A call either gate refuses is never sent; a refused
  compaction is skipped with a warning, and a refused sampling request returns the budget or
  spend error unchanged. `ModelCallScope::for_run` builds the scope from a `RunConfig`,
  `ModelCallScope::generate` meters one call, `ModelCallScope::within` makes it current for a
  future, and `generate_metered` and the `MeteredLlm` decorator meter against the current
  scope. `ModelSpendHold` is the shared reserve/commit/release step.
- **Spend caps are hard caps** (`adk-core`, `adk-agent`, `adk-model`): a model call reserves
  an upper bound on its cost, `Llm::max_cost_micro_usd`, so its committed cost never passes a
  cap. Every `adk-model` provider implements it from `PricingCatalog::max_cost_micro_usd` —
  the input's byte length (`request_input_token_bound`) at the highest input-side rate plus
  the request's output cap, or the model's published output limit
  (`ModelPrice::max_output_tokens`, set for Gemini, OpenAI, DeepSeek, Groq and Mistral
  models), at the output rate, plus built-in tool fees. A call the model cannot bound
  (unpriced model, no output cap and no published limit, `FileData` input) reserves
  `UNBOUNDED_HOLD_MICRO_USD`, which any covering cap refuses. The trait default is `None`;
  `Llm` wrappers forward it.
- **Sampling scopes** (`adk-tool`, `mcp-sampling`): `McpToolset::with_sampling_scope` and
  `AdkClientHandler::with_sampling_scope` attach a budget and spend ledger to sampling
  requests that arrive outside any run's tool call; `SamplingScopes` resolves the run a
  request counts against.
