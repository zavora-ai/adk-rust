- **Realtime refuses malformed tool arguments** (`adk-realtime`): arguments that were not
  valid JSON were replaced with `{}` and the tool ran anyway. The call is now answered with
  an error and the tool does not run; a `transfer_to_agent` call without a readable
  `agent_name` is answered instead of transferring to an empty name.
