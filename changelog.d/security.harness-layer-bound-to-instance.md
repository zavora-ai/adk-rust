- **The harness's governance check is bound to its own layer** (`adk-harness`): a run
  policy that merely names itself `adk-harness` no longer stands in for the harness. The
  layer composes the harness policy with it instead of skipping, a nested harness refuses
  to run under it (`harness.ungoverned`), and `has_harness_layer`, which `bash` and
  `exit_plan_mode` check, is true only for a run policy a harness layer composed. Before,
  such a policy let host `bash` run unapproved in Ask mode (spec 3.1 §7.3, §9.2 I17).
