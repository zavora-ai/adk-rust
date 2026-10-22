- **Review boards as runner roots** (`adk-harness`, spec 3.1 §7.10):
  `HarnessTeam::review_round_with(name, reviewers, policy)` builds a round-robin review
  round under a whole `BlackboardPolicy` (rounds, history, `TeamBudget`, termination), and
  `HarnessAgent::governed_root(agent)` runs it as a `Runner` root under the harness layer:
  every member call goes through `authorize_tool_call` under the harness policy, the members'
  `harness.member.*` records land in the runner's audit trail, the run is metered into
  `HarnessRuntime::spend`, and the root refuses to run without the layer.
