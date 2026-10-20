- **Harness observability and self-correction** (`adk-harness`, feature `observe`; spec 3.1
  §7.13): `harness.run` and `harness.decide` spans carry `harness.mode`,
  `harness.tool_kind`, `harness.isolation`, `harness.rule_id` and `harness.member`, with
  `member_span` and `goal_iteration_span` for members and goal iterations.
  - Every harness run is metered in a `SpendLedger` (an `InMemorySpendLedger` unless one is
    configured) with spend attributed per acting agent and subject; `Harness::spend`
    returns a `SpendReport` (`render()` is the CLI's `/cost`), and each run records
    `harness.spend`.
  - `HarnessAgentBuilder::loop_guard(LoopGuard { window, max_identical_calls, max_redundant })`
    runs `adk-eval`'s `TraceAnalyzer` before each model request; a loop records
    `harness.loop.detected`, tightens the session to ReadOnly (`SystemTighten`) and ends the
    turn with `harness.loop_detected`.
  - An `adk-eval` suite (`fix_failing_test`, `plan_mode_writes_nothing`, `goal_until`) with a
    `BaselineStore` baseline runs on the scripted model in the PR tier and on live models in
    the nightly `harness-eval-live` job (`examples/harness_eval`).
