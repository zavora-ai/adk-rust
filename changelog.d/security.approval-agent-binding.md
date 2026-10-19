- **Approvals bind to the agent that made the call and the tool it was made to**
  (`adk-core`, `adk-agent`, `adk-tool`, `adk-acp`, `adk-payments`): a
  `RunConfig::tool_confirmation_fingerprints` binding named only the tool and its arguments,
  so a call-ID decision replayed onto another agent's call with the same name and arguments,
  or onto a same-named tool another MCP server publishes, authorized it. The fingerprint now
  names the acting agent's `AgentId` and the tool's `ToolOrigin` (its publisher and a SHA-256
  digest of its declaration and annotations), and every decision source keys on it:
  call-ID bindings, `RunConfig::tool_approvals`, every `ApprovalStore` backend, the approval
  service, and managed and durable-worker decision keys. A held call that resumes for another
  agent, or against a tool with another origin or declaration, is answered as not run with
  the reason, and its stored decision is spent. The ACP server binds every client decision to
  the request's fingerprint.
