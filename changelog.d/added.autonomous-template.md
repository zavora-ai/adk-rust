- **`autonomous` template** (`cargo-adk`): an agent that works toward goals and wakes on a
  weekday cron trigger on the agent kernel, with every store in `.adk/state.db`, `auto` mode
  with the OS sandbox, an approval-gated `publish_status` side effect, a per-run budget, a
  daily spend cap and a signed audit log. The `harness` template keeps its approvals, action
  ledger, standing rules and kernel stores in `.adk/state.db` instead of memory, and builds on
  `autonomy`.
