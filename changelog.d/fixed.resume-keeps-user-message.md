- **A resumed run keeps the person's message** (`adk-agent`): a run that resumes held tool
  calls with a user turn without parts — as `Harness::resume` and a `/run_sse` request
  without parts send — took the place of the person's last message, so the model received
  the call and its result but not the request. An empty user turn, with or without blank
  text, now adds nothing and replaces nothing, for an `LlmAgent`, a workflow sub-agent and
  the harness, and after a call held for want of a handler or one that timed out and waited
  in the approval store.
- **A current-turn agent sees the call it resumes** (`adk-agent`): an `LlmAgent` with
  `IncludeContents::None` sent its model only the run's user turn, so a resumed call ran but
  its result was dropped, and an empty resume turn reached the model as an empty request. A
  run that resumes held calls now sends the turn from the message that led to the earliest
  one — that message, the call and its result — and an empty turn without held calls sends
  the person's last message and what followed it.
