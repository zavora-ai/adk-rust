- **`BenchmarkResult` reports only what the runner measures** (`adk-bench`): `BenchRunner`
  never filled `tool_invocation` or `token_overhead`, so both fields and their
  `ToolInvocationMetrics`, `TokenOverheadMetrics` and `TokenBreakdown` types are removed;
  external harnesses still report token overhead through the EBP output. `memory` is now
  filled: the peak RSS of the measurement phase, sampled every
  `memory_sample_interval_ms`, and the growth per concurrent agent, on Linux and macOS.
  `BenchmarkResult::model` and the cost estimate use the model the run called
  (`BenchConfig::model`) instead of the workload's declared model.
