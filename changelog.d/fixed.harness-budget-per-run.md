- **A harness budget bounds each run on its own** (`adk-harness`, spec 3.1 §7.13): a
  `RunBudget` set with `HarnessAgentBuilder::budget` was carried into the runner's base
  config with one `BudgetTracker`, so every run of a runner built with
  `HarnessAgent::configure_runner` shared the counters and a run that made no tool call
  reported the previous run's usage. The base config now carries the budget only; the
  runner counts every run against fresh counters.
