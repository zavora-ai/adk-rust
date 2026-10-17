- **`held_call_resume` example** (`examples/held_call_resume`): real `LlmAgent`s in
  `SequentialAgent`, `LoopAgent`, `ParallelAgent` and `LlmConditionalAgent`, an `AgentTool`,
  session compaction with overlap 0 and 1, SQLite and in-memory sessions, and the durable worker,
  against a scripted upstream serving the real `OpenAIClient` and `OllamaModel` (calls without
  IDs) and a live OpenAI scenario. It counts every model call and tool execution and asserts that
  a held call survives compaction and runs once, that workflows stop at a hold and never re-run
  finished sub-agents, that same-name calls without IDs keep their arguments, and that the
  tool-call budget counts a held call once.
