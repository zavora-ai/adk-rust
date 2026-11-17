- **Rubric-graded goals** (`adk-eval`, `adk-harness`, `adk-cli`, `cargo-adk`): a goal can be
  checked by a grader in a context of its own instead of, or as well as, an `until` command.
  `adk_eval::OutcomeGrader` grades a deliverable per criterion with feedback in one model
  call; `adk_eval::RubricCheckAgent` ends a `LoopAgent` once the rubric passes or its cap
  (default 3, at most 20) is reached. `GoalSpec::with_rubric(GoalRubric)` grades each harness
  goal iteration's final message and deliverable files, feeds the failed criteria to the next
  iteration, counts the grader's call against the run budget and spend ledger, and audits each
  round as `harness.goal.grade`. `adk-rust goal` takes `--rubric FILE`, `--rubric-from-goal`,
  `--deliverable`, `--max-evaluations` and `--rubric-hidden`, or a `[goal]` table in
  `.adk/harness.toml`; the `autonomous` template grades its goals against `rubric.md`.
