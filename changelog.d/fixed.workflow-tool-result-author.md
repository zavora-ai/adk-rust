- **Workflow sub-agents author their own tool results** (`adk-agent`): under a
  `SequentialAgent`, `ParallelAgent`, or `LoopAgent`, an `LlmAgent` authored its tool results
  and tool-started events with the workflow's name instead of its own. They are authored by
  the agent that issued the call, so a call and its result share an author in the session and
  a resumed held call's result releases its hold.
