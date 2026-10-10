- **`cargo adk eval` exits non-zero instead of reporting unexecuted cases as passed**
  (`cargo-adk`): the command never ran an agent, so every case, JUnit report, and
  `--check-regression` run came out green. It now loads the eval set, states that no
  case was executed, and exits with status 1 whatever flags are passed. Run eval sets
  from an integration test with `adk_eval::Evaluator`, as shown in "Running Evaluations
  in CI" in the evaluation guide.
