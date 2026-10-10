- **Tool argument matching is recursive** (`adk-eval`): `ToolUse::matches` compares
  nested objects as subsets when `strict_args` is `false` and as exact key sets when it
  is `true`, compares numbers by value so `1` matches `1.0`, and treats an expected tool
  use without `args` as matching any arguments. `ToolTrajectoryScorer::compare` honours
  `strict_order`, so its matched, missing, and extra calls agree with `score`.
- **Multi-turn case scores are the mean of turn scores** (`adk-eval`): a criterion's case
  score is the arithmetic mean across turns instead of a running pairwise average that
  weighted later turns exponentially.
