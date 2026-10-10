- **Tool policy with default deny** (`adk-core`, `adk-runner`): `ToolPolicy` decides every
  tool call — allow, deny, or require approval — on its final arguments, through
  `RunConfig::tool_policy` or `Runner::builder().tool_policy(...)`, and travels into transfer
  targets and agents behind an `AgentTool`. `DeclarativePolicy` matches rules in order on a
  tool-name glob, the tool's declared `ToolEffect` (`read_only_tools`, `with_effect`), and
  JSON-pointer argument predicates (`equals`, `in_set`, `at_most`, `starts_with`,
  `domain_in`); a tool no rule permits is denied. `ToolPolicyRequest::effect` carries the
  tool's `Tool::effect()`.
- **One governed execution path** (`adk-core`): `authorize_tool_call` runs the kill switch,
  the policy, the agent's guardrails, and confirmation in that order. `LlmAgent` and
  `CodeActAgent` call it after before-tool plugins and callbacks and immediately before
  `Tool::execute`.
- **Durable approvals** (`adk-core`): a decision binds to the call's fingerprint
  (`ToolConfirmationRequest::fingerprint`), so it authorizes the same call re-issued under a
  new call ID in a later run. `RunConfig::tool_approvals` carries fingerprint decisions with
  an optional expiry; `ApprovalStore` and `InMemoryApprovalStore` hold pending requests
  between runs and consume a decision with the call it authorizes.
- **Kill switch** (`adk-core`, `adk-runner`): `GovernanceControl::freeze` fails new runs at
  their start and ends running ones before their next model call or tool call with a
  non-retryable `governance.frozen` error. Every runner has one (`Runner::governance`);
  `Runner::builder().governance(control)` shares one across runners.
