- **Regression checks fail closed** (`adk-eval`): `BaselineStore::check_regressions` returns
  `EvalError::BaselineError` when the baseline file does not exist, instead of an empty list, so
  a mistyped path or an uncommitted baseline no longer passes a regression gate; save one with
  `BaselineStore::save` before checking against it. Every baseline metric and case without a
  score in the current run, such as a case that errored, is a regression, and so is a NaN score.
  `Regression::current_value` is `Option<f64>`, `None` for a missing score, and regressions are
  sorted by metric and case.
