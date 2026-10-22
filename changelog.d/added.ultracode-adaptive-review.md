- **Adaptive `ultracode`** (`adk-cli`, spec 3.1 §7.10, §8.1): after the implementer's first
  turn, `adk-rust ultracode` routes the review once, on the actual change, to parallel
  independent reviewers (`graph`) or one round-robin Blackboard board whose reviewers read
  each other (`blackboard`); reviewers are ReadOnly and shell-less in both.
  - Routing (`ultracode-route/1`): an operator override, then `unchanged` (the fixed review),
    then `sensitive` (a protected, security-vocabulary or user-config `sensitive_paths`
    change, or a workspace too large to see whole, goes to a board with a `security` seat),
    then one closed-schema call at the session's `members` effort, validated whole, with a
    deterministic heuristic as the fallback for any failure or invalid answer. A
    workspace's own `sensitive_paths` add the `security` seat without choosing the board.
  - Every call takes its effort from the session's `EffortPolicy` (`--effort`, `[effort]`):
    independent reviewers `read_only`, board seats and the router `members`
    (`IndependentReviewer::effort`, `ReviewBoard::effort`).
  - A revise turn that makes the change sensitive adds the `security` seat to the stages
    after it (`harness.ultracode.escalated`, `replay_escalation`); the revise is reserved
    against the budget at that size.
  - The change snapshot sees `.git/config`, `.git/hooks/` and `.git/info/`, enters build and
    dependency directories that a `sensitive_paths` glob names, and walks breadth-first.
  - `--receipt` refuses a link inside the workspace on the way to the file, or a directory
    that resolves elsewhere than when the run started, and never writes through a link.
  - Flags `--architecture auto|graph|blackboard`, `--router model|heuristic`,
    `--max-model-calls N` and `--receipt FILE`; an `[ultracode]` table in the user config
    and in `.adk/harness.toml`, which may only tighten.
  - `harness.ultracode.started`, `.routed`, `.escalated`, `.reviewed` and `.finished` records
    in one signed audit log per run; `adk_cli::harness::ultracode::replay` recomputes the
    plan from the `routed` record.
  - `adk_cli::harness::ultracode` exposes the router, the signals, the limits,
    `ReviewBoard` and `IndependentReviewer` (Beta); `EphemeralAuditLog::records` reads a
    log's records back (`adk-harness`).
