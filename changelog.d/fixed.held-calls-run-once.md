- **Held tool calls survive compaction, workflows and providers without call IDs, and run once**
  (`adk-core`, `adk-agent`, `adk-runner`, `adk-tool`, `adk-model`):
  - A call held before a compaction is still resumed by its approval, and a durable worker run
    holding it parks as `Waiting` instead of completing with the confirmation notice while its
    approval stays pending.
  - Two calls to one tool without IDs in one turn keep their own arguments; an approval for one
    runs exactly that one, and a response to one no longer releases the other.
  - An `AgentTool` call without an ID whose delegate held a call resumes the delegation instead
    of being answered "tool not found".
  - A later workflow sub-agent, and an `AgentTool` delegate's rebuilt session, see a resumed
    call's result in place of its pending response instead of both.
  - A held call no longer counts against the run's tool-call budget when it is held and again
    when it resumes (`BudgetTracker::release_tool_calls`).
  - `OllamaModel` streams tool calls as partial responses, so a streamed tool-call turn is saved
    as one event instead of two events with the same ID, which a SQLite or Postgres session
    rejected.
