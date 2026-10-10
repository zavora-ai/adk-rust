- **Resumed sub-agents keep their transfer targets** (`adk-runner`): when the runner
  resumes the sub-agent that answered last, it gives that agent its parent and peers as
  transfer targets, as it does when control is transferred to it. A resumed `LlmAgent`
  previously had no `transfer_to_agent` tool and answered every later request itself, and
  it now reads the same agent-scoped history on both paths.
