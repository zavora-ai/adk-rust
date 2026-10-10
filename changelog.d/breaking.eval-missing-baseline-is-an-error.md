- **A missing regression baseline is an error** (`adk-eval`):
  `BaselineStore::check_regressions` returns `EvalError::BaselineError` when the baseline
  file does not exist, instead of an empty list, so a mistyped path or an uncommitted
  baseline no longer passes a regression gate. Save a baseline with
  `BaselineStore::save` before checking against it.
