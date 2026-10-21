- **Goal runs no longer take other durable runs, and the goal check honours its timeout**
  (`adk-harness`, `adk-cli`, spec 3.1 §7.11):
  - `Harness::with_goal_queue` and `GOAL_QUEUE` (`"harness.goal"`) put durable goals on a
    queue of their own. `adk-rust goal` uses it, so its worker no longer claims, and fails,
    a background run that `adk-rust serve` queued in the same `harness.db`.
  - `UntilCheckAgent` runs the check for up to `GoalSpec::check_timeout`, above the
    workspace's shell limit for the model's own commands; `adk-rust goal --check-timeout
    SECS` sets it (600 by default).
  - `adk-rust approvals approve` prints the `adk-rust goal … --resume` command when the call
    belongs to a goal run parked on it.
  - `HarnessAgentBuilder::loop_guard` documents what the guard does instead of calling it
    not yet implemented.
