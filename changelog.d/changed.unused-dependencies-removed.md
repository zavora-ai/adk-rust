- **Unused dependencies removed** (20 crates): `adk-audio/livekit` no longer links
  `livekit-api` and `adk-realtime`, which `RealtimeBridge` never used (161 to 93 crates);
  `adk-realtime/openai` no longer links `async-openai` (156 to 117 crates); `adk-managed`
  drops `adk-skill` and `adk-telemetry`, and its `memory` feature no longer links
  `adk-memory`, since it takes any `adk_core::Memory`; `adk-harness/observe` no longer links
  `adk-telemetry`; `adk-eval/statistics` computes the Wilcoxon p-value with `libm`'s `erfc`
  (already linked by `experiments`) instead of `statrs` (120 to 99 crates); p-values move by
  less than 2e-10 relative, toward the exact value. The remaining unused or test-only
  entries move to `[dev-dependencies]` or go. The workspace `tokio-tungstenite` moves to
  0.29, `adk-server` to `tower` 0.5, and `adk-sandbox` to `windows-sys` 0.61, so ADK's own
  manifests ask for one version of each. No API change.
