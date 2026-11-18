- **Repeated eval runs with pass rates and flakiness** (`adk-eval`, `cargo-adk`):
  `Evaluator::evaluate_test_file_runs` runs an eval set `num_runs` times, each case in a fresh
  session, and `MultiRunReport` gives each case's pass rate, the mean, sample variance, minimum
  and maximum of every criterion's score, and whether it is flaky; a case passes when it reaches
  `MultiRunConfig::min_pass_rate` (every run by default). `MultiRunReport::baseline_metrics`
  adds a `pass_rate` metric, so a regression check catches a case that became flaky.
  `cargo adk eval` takes `--num-runs`, `--min-pass-rate`, `--judge-max-calls` and
  `--judge-max-cost-usd`, and a baseline it saves holds `pass_rate`. `Evaluator::set_run_config`
  meters the evaluation: every eval run and every judge call counts against the config's budget
  and spend ledger (judge spend under the `eval-judge` agent) and stops when its kill switch is
  frozen. See `docs/official_docs/evaluation/evaluation.md`.
