- **Confirmation handlers can edit a call and explain a denial** (`adk-core`):
  `ToolConfirmationHandler::decide_approval` is a provided method returning a
  `ToolApproval`; the governed path calls it instead of `decide`. The default maps `decide`'s
  answer to `ToolApproval::approve()` or `deny()`. An approval with `edited_args` executes the
  call with them after the `ToolPolicy` and the agent's guardrails check them again; a
  denial's `message` is reported to the model.
