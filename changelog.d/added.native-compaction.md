- **Provider-native compaction** (`adk-core`, `adk-runner`, `adk-model`, `adk-anthropic`):
  auto-compaction has the model's provider write the summary when the model reports
  `Llm::native_compaction`, and keeps the provider's signed result as the first history entry,
  replayed only to the provider and model that wrote it. `AnthropicClient` reports it on the
  Claude API for the models that compact on demand (beta `compact-2026-09-04`): the compaction
  request carries the agent's instructions, tools and config, later requests send the signed
  `compaction` block first, and the kept turns keep their thinking. The call counts against the
  run's budget and spend ledger, a frozen run refuses it, and a call held for confirmation stays
  out of it; a provider compaction that fails falls back to `LlmEventSummarizer`.
  `adk-anthropic` gains `CompactionParam`, `CompactionBlock`, `MessageCreateParams::compaction`
  and `requires_compaction_beta`, and adds the beta header itself.
