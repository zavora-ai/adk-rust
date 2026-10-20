- **An agent tool surfaces every call its delegate holds** (`adk-tool`, `adk-agent`): a
  delegate that held several calls in one turn surfaced only the first to its caller, and a
  call left undecided when the delegation resumed was lost with the delegate's session.
  `AgentTool` now surfaces one `ToolConfirmationRequest` per held call, each under the
  caller's call ID, and keeps the delegation held while any call is undecided; `LlmAgent`
  resumes the agent-tool call once however many of its requests have decisions. A call-ID
  decision without a fingerprint binding reaches the delegate only when it holds one call.
  Implements spec 3.1 §10 F4.
