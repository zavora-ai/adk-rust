- **An agent behind an `AgentTool` runs as its caller's delegate** (`adk-tool`, `adk-agent`):
  the delegate no longer inherits its caller's `transfer_targets`, `parent_agent`,
  `cached_content`, `runtime_toolsets` or call-ID confirmation decisions; it gets its own
  topology from `Agent::configure_run` and `Agent::transfer_targets_for`, and shares the
  caller's budget, governance, policy, approvals, ledgers, audit trail and acting chain. A
  delegate that ends on a tool result — a tool that skips summarization, or a nested
  `AgentTool` with `skip_summarization(true)` — hands back that result instead of
  `"No response from agent"`.
- **Calls held inside an `AgentTool` reach the caller** (`adk-tool`, `adk-agent`, `adk-core`):
  the delegate runs in the caller's approval scope, and a call it holds for confirmation holds
  the caller's `AgentTool` call with a `ToolConfirmationRequest` for the delegate's call. Once
  that request has a decision, the caller resumes its call, the delegate resumes its own held
  call from the session it stopped in, and the call runs once. A tool marks its call as held by
  setting `EventActions::tool_confirmation`.
- **Forwarded events survive a slow client** (`adk-agent`): `ToolContext::emit_event` waits for
  the client to take a complete event, as tool results do, so `AgentTool::forward_events(true)`
  no longer drops session history after 100 ms. A dropped partial event is logged and reported
  by one `[adk: forwarded events dropped]` progress event.
