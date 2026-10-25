- **`capability_questions` in the harness eval suite** (`adk-harness`, `examples/harness_eval`):
  a mistyped slash command, resuming a conversation and a standing denial for a directory are
  asked of a harness with the `adk-rust` commands as its surface, and pass when the answer
  names `/approvals`, `--continue` and `rules add edit_file … --deny`; the nightly
  `harness-eval-live` job runs them with the scenarios (spec 3.1 §7.25).
