- **A call with an unknown outcome is no longer held for its delegate's request**
  (`adk-agent`, `adk-realtime`): a `NonIdempotent` tool that times out or panics after it set a
  `ToolConfirmationRequest` — an agent tool whose delegate held a call — is answered
  `outcome_unknown` instead of being held to run again on a decision, and the request is
  withdrawn from the approval store and service, so no later decision on it authorizes an
  identical call. `LlmAgent`, `RealtimeAgent` and `IntegratedRealtimeRunner` agree.
