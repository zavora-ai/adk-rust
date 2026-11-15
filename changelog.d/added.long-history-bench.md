- **Keyless long-history benchmark** (`adk-bench`): `long_history` drives an `LlmAgent` through
  `Runner` with a scripted, zero-latency model for N turns or N tool iterations and reports the
  per-step CPU time on a short and a full history, its growth, and peak RSS
  (`cargo run -p adk-bench --release --example long_history -- turns 1000`). An allocation test
  in the PR suite fails when a history window stops bounding a turn or a turn copies the
  history more often.
