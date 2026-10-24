- **A resumed run keeps the person's message** (`adk-agent`): a run that resumes held tool
  calls with a user turn without parts — as `Harness::resume` and a `/run_sse` request
  without parts send — took the place of the person's last message, so the model received
  the call and its result but not the request. An empty user turn, with or without blank
  text, now adds nothing and replaces nothing, for an `LlmAgent`, a workflow sub-agent and
  the harness, and after a call held for want of a handler or one that timed out and waited
  in the approval store.
