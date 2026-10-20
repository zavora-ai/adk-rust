- **`adk_core::confirm_tool_call`** (`adk-core`): decides a confirmation a tool asks for
  while it executes, from the governed path's sources in order (call-ID decision,
  fingerprint approval, `ApprovalStore`, confirmation handler). Without a decision it
  records the request as pending and submits it to the run's `ApprovalService`, and the tool
  holds its call by setting `EventActions::tool_confirmation`.
