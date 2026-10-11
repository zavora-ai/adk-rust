- **Resuming a held call after a later model answer** (`adk-agent`): a run with no user
  input whose conversation ends on a model message — a held call resumed after the model
  already replied in a later run — now ends the request with a short user turn, instead of
  a model message that Anthropic models without prefill support reject.
