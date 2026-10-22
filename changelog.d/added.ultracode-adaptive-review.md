- **Adaptive `ultracode`** (`adk-cli`, spec 3.1 §7.10, §8.1): after the implementer's first
  turn, `adk-rust ultracode` routes the review once, on the actual change, to parallel
  independent reviewers (`graph`) or one round-robin Blackboard board whose reviewers read
  each other (`blackboard`); reviewers are ReadOnly and shell-less in both.
  - Routing (`ultracode-route/1`): an operator override, then `unchanged` (the fixed review),
    then `sensitive` (a protected, security-vocabulary or `sensitive_paths` change goes to a
    board with a `security` seat), then one closed-schema call to the member model, validated
    whole, with a deterministic heuristic as the fallback for any failure or invalid answer.
  - Flags `--architecture auto|graph|blackboard`, `--router model|heuristic`,
    `--max-model-calls N` and `--receipt FILE`; an `[ultracode]` table in the user config
    and in `.adk/harness.toml`, which may only tighten.
  - `harness.ultracode.started`, `.routed`, `.reviewed` and `.finished` records in one signed
    audit log per run; `adk_cli::harness::ultracode::replay` recomputes the plan from the
    `routed` record.
  - `adk_cli::harness::ultracode` exposes the router, the signals, the limits,
    `ReviewBoard` and `IndependentReviewer` (Beta); `EphemeralAuditLog::records` reads a
    log's records back (`adk-harness`).
