- **A workflow sub-agent's resumed call runs once** (`adk-agent`): under a `SequentialAgent`,
  `ParallelAgent`, or `LoopAgent`, an `LlmAgent` authored its tool results with the workflow's
  name, so a resumed held call's result never released the hold. With a run-wide approval,
  a call-ID decision, or a confirmation handler the call executed again in every later run.
  Tool results and tool-started events are now authored by the agent that issued the call.
