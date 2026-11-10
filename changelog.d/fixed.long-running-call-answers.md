- **A late answer to a long-running call reaches the model** (`adk-runner`, `adk-agent`):
  when a caller resumes a long-running call by sending its result, the history read as a
  second response to an answered call and dropped it, so the model never saw the answer.
  The history now ends on the call and the caller's result, as a `function` turn, leaving
  out the interim response and the model's reply to it, as ADK Python and ADK Go do. Only
  calls an event marks long-running are answered this way. `LlmAgent` no longer lets a turn
  of function responses replace the person's last message in the request.
