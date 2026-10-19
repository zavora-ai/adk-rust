- **`AgentTool` returns the answer of a delegate that has an `output_key`** (`adk-tool`): the
  state-only event `LlmAgent` emits after its answer to record `output_key` was taken as the
  delegation's final event, so the tool returned `{"response": "No response from agent"}`.
  Events without content are now skipped when the answer is read.
