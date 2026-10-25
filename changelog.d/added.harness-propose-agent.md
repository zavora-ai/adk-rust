- **Agents a person adds, in the harness** (`adk-harness`): `propose_agent` (class `Todo`, on a
  harness given `HarnessAgentBuilder::agents_handle`, the primary agent only) suggests an agent
  within the suggesting agent's own limits (`suggester_ceiling`, `Harness::agent_ceiling`),
  its instructions screened and redacted; only a person adds it. `HarnessAgentBuilder::added_agent`
  builds an added agent's harness from its `AgentRecord` — the stricter mode, no members,
  goals or `propose_agent`, the workspace narrowed to its folder, a deny layer for what it may
  not use — and `hosted_beside` shares the primary agent's approval service and agenda.
  `session_stamp` and `AGENT_STATE_KEY` keep each thread with one agent. `PinnedOnlyRules`,
  `RuleSuggestion::into_pinned_rule` and `copy_rule_to` keep an added agent to the rules pinned
  to its own id. `assess_agent` is the risk table of spec 3.3 §5.7.
