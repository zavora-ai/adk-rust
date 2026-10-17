- **Workflow sub-agents see other agents' turns as context** (`adk-core`, `adk-runner`,
  `adk-agent`, `adk-tool`): an `LlmAgent` that did not receive control by transfer now sends
  another agent's text, calls and results as user-role context — one message per event,
  opening with `adk_core::history::OTHER_AGENT_CONTEXT_PREAMBLE` and quoting each payload
  between `<<<BEGIN_QUOTED_AGENT_CONTENT>>>` and `<<<END_QUOTED_AGENT_CONTENT>>>` — instead of
  replaying them as its own model turns, and leaves their thinking out. A history that would
  end on the agent's own answer gets a user turn. `SequentialAgent[gemini, claude-sonnet-4-6]`
  no longer fails with Anthropic's "does not support assistant message prefill". New
  `Session::conversation_history_with_context` and `Session::records_invocation_events`
  (defaults keep the old behaviour); `LoopAgent` reads history from a session that records
  every event. Events `AgentTool::forward_events(true)` forwards carry the tool's branch
  (`adk_core::history::tool_branch`), and the caller's history leaves them out. Code that
  matched the old request shape needs updating.
