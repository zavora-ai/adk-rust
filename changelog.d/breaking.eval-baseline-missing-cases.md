- **Baseline cases missing from a run are regressions** (`adk-eval`):
  `BaselineStore::check_regressions` reports every baseline metric and case that has no
  score in the current run, such as a case that errored, and treats a NaN score as a
  regression. `Regression::current_value` is `Option<f64>`, `None` for a missing score,
  and regressions are sorted by metric and case.
