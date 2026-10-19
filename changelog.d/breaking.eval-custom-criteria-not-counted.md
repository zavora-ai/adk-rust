- **Custom criteria alone do not count as evaluation criteria** (`adk-eval`, `cargo-adk`):
  `EvaluationCriteria::has_criteria` counted `custom` entries, which `Evaluator` never
  scores, so a configuration with only custom criteria ran the agent and failed every case
  as `not_evaluated` instead of being rejected as empty. `has_criteria` now leaves them out,
  `evaluate_case` and `evaluate_test_file` return `EvalError::ConfigError` for such a
  configuration before the agent runs, and `Evaluator` logs a warning when custom criteria
  sit beside scored ones. Score custom criteria in your own code from
  `EvaluationCriteria::custom`.
