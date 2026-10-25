- **Standing rules are named once and announced when they apply** (`adk-harness`, `adk-cli`):
  a rule suggestion's label states what it matches and never its scope, so the console no
  longer offers "edit_file: anywhere in the workspace (this session) (this session)"; and a
  call a rule allows where the mode would ask is announced — the policy and the handler's
  re-check call the new provided `ApprovalPrompter::rule_applied` with a `RuleHit`, and the
  console prints `[allowed by rule N] <tool>: <label>`, including for a call a rule granted
  earlier in the same batch covers (spec 3.1 §7.33 fixes 1 and 3).
