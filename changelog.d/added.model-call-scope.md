- **`ModelCallScope`** (`adk-core`): applies a run's budget and spend ledger to every model
  call made on behalf of the run. `ModelCallScope::for_run` builds it from a `RunConfig`,
  `ModelCallScope::within` makes it current for a future, and `generate_metered` and the
  `MeteredLlm` decorator meter against the current scope. `ModelSpendHold` is the shared
  reserve/commit/release step.
- **`Llm::max_cost_micro_usd`** (`adk-core`, `adk-model`): an upper bound on a request's
  cost, implemented by every `adk-model` provider from `PricingCatalog::max_cost_micro_usd`,
  `request_input_token_bound` and `ModelPrice::max_output_tokens` (published output limits
  for Gemini, OpenAI, DeepSeek, Groq and Mistral models). The default is `None`.
- **Sampling scopes** (`adk-tool`, `mcp-sampling`): `McpToolset::with_sampling_scope` and
  `AdkClientHandler::with_sampling_scope` attach a budget and spend ledger to sampling
  requests that arrive outside any run's tool call; `SamplingScopes` resolves the run a
  request counts against.
