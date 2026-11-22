- **A missing instruction placeholder is an `InvalidInput` error that says how to keep
  braces literal** (`adk-core`): a required `{key}` absent from session state fails with code
  `agent.instruction.state_missing` instead of `agent.legacy` (`Internal`), and the message
  names `{key?}`, `static_instruction` and the `{{key}}` and `\{key}` escapes.
