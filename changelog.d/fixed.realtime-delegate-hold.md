- **Realtime runners surface and resume held calls** (`adk-realtime`): a call the governed
  path holds, or that a tool's delegate holds (an `AgentTool` whose agent held a call),
  was answered to the model as a plain error and lost. `IntegratedRealtimeRunner` now
  answers it as `confirmation_pending`, records the hold in the session under
  the runner's agent name and in the approval store, emits
  `ServerEvent::ToolConfirmationRequested`, and runs it once decided through
  `resume_held_calls()`; `held_calls()` lists what waits. `RealtimeAgent` holds its own call
  when its delegate holds one and resumes decided calls at the start of a run, as
  `LlmAgent` does. Tool contexts carry the governance `RunConfig` and acting context, so a
  tool that checks its run's policy sees the one the gate applied. `agent_name` on
  `RealtimeRunnerBuilder` and `IntegratedRealtimeRunnerBuilder` names the agent calls are
  governed, metered and held under (default `realtime`).
