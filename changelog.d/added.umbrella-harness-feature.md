- **`harness` feature on the umbrella crate** (`adk-rust`, spec 3.1 §8.4): links
  `adk-harness` with `agents`, `tools`, `skills`, `sandbox`, `runner`, `sessions` and
  `graph`, and is part of `full`. `adk_rust::harness` re-exports `adk-harness`, plus the
  CLI's `HarnessLauncher`, `ConsolePrompter` and `ConsoleIo` when `cli` is also on; the
  prelude gains `HarnessAgent`, `HarnessAgentBuilder` and `HarnessMode`. The packs forward
  as `harness-browser`, `harness-computer-use`, `harness-payments`, `harness-voice` and
  `harness-managed`.
