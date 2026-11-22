- **A missing instruction placeholder is an `InvalidInput` error that says how to keep
  braces literal** (`adk-core`): a required `{key}` absent from session state fails with code
  `agent.instruction.state_missing` instead of `agent.legacy` (`Internal`), and the message
  names `{key?}`, `instruction_provider` (sent as written) and the `{{key}}` and `\{key}`
  escapes. The LLM agent guide shows a skill file sent through `instruction_provider`.
