- **Approvals bind to the agent that held the call and the tool it was held for**
  (`adk-core`, `adk-agent`, `adk-tool`, `adk-acp`, `adk-payments`): a decision was keyed by
  tool name and arguments only, so in `Sequential[b, a]` an approval of `a`'s held
  `transfer {"amount": 40}` executed `b`'s own `transfer` with the same arguments while `a`'s
  call stayed held, and a held MCP call resumed on whichever server published the tool's name
  at resume time, or on a tool the server had redefined since. The fingerprint now names the
  acting agent's `AgentId` and the tool's `ToolOrigin` (its publisher and a SHA-256 digest of
  its declaration and annotations), which binds run-wide approvals, every `ApprovalStore`
  backend, the approval service, managed and durable-worker decision keys, and call-ID
  decisions bound through `tool_confirmation_fingerprints`. A held call that resumes for
  another agent, or against a tool with another origin or declaration, is answered as not run
  with the reason and its stored decision is spent. The ACP server binds every client decision
  to the request's fingerprint.
