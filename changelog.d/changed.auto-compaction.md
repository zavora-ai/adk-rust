- **Auto-compaction before every model call, on by default** (`adk-core`, `adk-runner`,
  `adk-agent`, `adk-model`, `adk-harness`, `adk-cli`): a long autonomous run no longer grows its
  request until the provider rejects it. `LlmAgent` and `CodeActAgent` pass their history to
  the run's `HistoryCompactor` (`RunConfig::history_compactor`, installed by the runner) before
  each model call; once the request passes 80% of the model's context window
  (`Llm::context_window`, from the vendors' published figures in
  `PricingCatalog::context_window`) or 100,000 estimated tokens, all but the 10 most recent
  entries are summarized by the agent's model and the compaction is recorded in the session. A
  request the model rejects as too long (`is_context_limit_error`) is compacted and sent once
  more. Change it with `RunnerConfigBuilder::intra_compaction_config`,
  `intra_compaction_summarizer`, `context_compaction` or `without_intra_compaction`,
  `RunConfig::intra_compaction`, `HarnessAgentBuilder::intra_compaction` and the `[compaction]`
  table of `harness.toml`.
  Entries a compaction keeps, in a run or in the session, are replayed without their thinking
  parts, which providers bind to the history they were produced with.
