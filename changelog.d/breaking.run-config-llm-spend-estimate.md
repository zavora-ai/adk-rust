- **`LlmSpendRecorder` is removed** (`adk-runner`): `LlmAgent`, `CodeActAgent`,
  `LlmConditionalAgent`, compaction and MCP sampling reserve and commit through
  `ModelCallScope`, and the runner no longer installs a spend hook. `LlmSpendEstimate`
  moved to `adk-core` (`adk_runner::LlmSpendEstimate` re-exports it) and now sets only what
  an unpriced call commits when it reports no cost.
- **`RunConfig::llm_spend_estimate`** (`adk-core`): new public field, so struct literals
  without `..Default::default()` need it. The runner fills it from
  `Runner::with_llm_spend_estimate` at the start of each run.
- **Unpriced models are refused under a spend cap** (`adk-core`): a call whose model returns
  no `Llm::max_cost_micro_usd` bound is refused wherever a `SpendLimits` entry covers it.
  Custom `Llm` wrappers must forward `max_cost_micro_usd`.
- **`PriceVendor` gains `Groq`, `XAi` and `Mistral`; `ModelPrice` gains
  `max_output_tokens`** (`adk-model`).
- **`LlmSamplingHandler` keeps budget and spend errors** (`adk-tool`): a request refused
  by the run budget or the spend ledger returns that error unchanged instead of wrapping
  it in a tool error.
