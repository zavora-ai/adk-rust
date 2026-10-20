- **An operator policy's `RequireApproval` reaches the person** (`adk-harness`): with a
  prompter configured, a call that `HarnessAgentBuilder::operator_policy` or the run's own
  policy requires approval for, and the harness itself allows, is put to the person
  instead of being refused with "the harness policy did not evaluate this call". The
  prompting handler re-checks a held call against the run's whole policy, so a standing
  rule answers only the harness's own approval: it never approves a call another layer
  requires approval for, even when the rule was granted earlier in the same batch, and no
  rule is offered at such a prompt (spec 3.1 §7.3 compose rule, §7.5).
