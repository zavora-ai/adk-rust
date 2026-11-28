- **Managed turns obey the operator's kill switch, tool policy and action ledger, and built-in
  commands no longer run unconfined on the host** (`adk-managed`): every turn's `Runner` shares
  the runtime's `GovernanceControl` — the operator's from `with_governance`, or the runtime's own
  from `governance()` — so a freeze refuses new turns and stops a running one before its next
  tool or model call. `with_tool_policy` and `with_action_ledger` reach every turn the same way.
  Built-in `bash` and `code_execution` run under the OS sandbox with an allowlisted
  environment, so a command no longer sees the runtime's provider keys or writes outside its
  session's workspace.
