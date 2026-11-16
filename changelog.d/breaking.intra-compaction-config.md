- **`IntraCompactionConfig` is the auto-compaction setting** (`adk-core`, `adk-runner`): it gains
  `enabled` and `context_window_fraction`, and `token_threshold` becomes `Option<u64>`, where
  `None` derives the threshold from the model's window. The runner no longer compacts session
  events once before the agent runs, nor retries `agent.run()` on a token-limit error: both
  happen inside the agent before each model call. `RunConfig` gains `intra_compaction` and
  `history_compactor`, `ModelPrice` gains `context_window_tokens`, so struct literals must add
  them. `LlmEventSummarizer` moves to `adk-core`; `adk_agent::LlmEventSummarizer` still names it.

  Migration: replace `IntraCompactionConfig { token_threshold: n, .. }` with
  `IntraCompactionConfig::at_tokens(n)`; an agent that calls a model itself passes its history
  to `ctx.run_config().history_compactor` before each call.
