- **`agent_tool_isolation` example** (`examples/agent_tool_isolation`): a coordinator that
  transfers to a specialist, which delegates to a helper through an `AgentTool`, against
  scripted OpenAI-compatible servers and a live OpenAI scenario. It asserts that the helper's
  request carries no `transfer_to_agent` tool, cache handle or invocation toolset, that a
  helper ending on a tool result hands back that result, that a delegation timeout is a failed
  call with category `Timeout`, that a helper's held call surfaces at the caller and runs once
  after approval, and that forwarded events survive a stalled client.
