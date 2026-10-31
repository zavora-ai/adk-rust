- **Refused, unrun and failed tool answers are flagged as errors** (`adk-model`): Anthropic
  `tool_result` blocks and Gemini Interactions `function_result` steps carry `is_error: true`
  when the agent answered the call as `refused`, `not_run` or `failed`. A tool's own result,
  and a held, withheld, succeeded or outcome-unknown answer, is sent without the flag.
