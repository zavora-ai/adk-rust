- **Evaluations that evaluate nothing no longer pass** (`adk-eval`):
  `evaluate_test_file` and `evaluate_file` return `EvalError::ConfigError` for a file
  without eval cases, `evaluate_case` for a case without turns, all three for an
  evaluator without criteria, and `evaluate_directory` for a directory without
  `*.test.json` files.
  A case that no configured criterion applies to fails with a `not_evaluated` failure,
  and `EvaluationReport::all_passed()` is `false` for a report without results.
- **`Evaluator::evaluate_cases_parallel` returns `Result<Vec<Result<EvaluationResult>>>`**
  (`adk-eval`): a concurrency of 0, which never ran a case and never returned, and an
  empty `cases` slice are `EvalError::ConfigError`. Results now follow the order of
  `cases`; add `?` at call sites.
