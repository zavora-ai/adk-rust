- **`adk-rust code` exits `1` when the run fails** (`adk-cli`): a run that ends on a model
  error or an exhausted budget prints `the run failed: <error>` last and exits `1`, so CI
  and unattended callers see it; `TurnOutcome::error` carries the error. `goal` prints the
  error before its exit `1`, and `ultracode` counts such a turn as failed.
