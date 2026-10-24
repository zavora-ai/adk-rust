- **Tool-result status** (`adk-core`, `adk-agent`, `adk-harness`, `adk-managed`, `adk-model`,
  `adk-realtime`): framework
  answers carry `ToolResultStatus` (`Failed`, `NotRun`, `Withheld`, `OutcomeUnknown`,
  `Pending`) in function-response annotations, with the error's code and category beside
  it. `ToolResultStatus::derive` never marks an entered `execute` as `NotRun`.
  `Part::tool_result_status`, `Part::tool_result_error_code`, `Part::tool_result_error_category`
  and `Part::set_tool_result_status` read and write this metadata without changing the
  response JSON. Replacement hooks start with fresh annotations; an existing unknown
  outcome retains its status, code and category through later processing.
  `CodeActCheckpoint::resolution_status` records a resolved call's non-success status,
  including unknown outcomes carried through replacement values; ordinary returned values
  and older checkpoints have no status.
  The Anthropic, Gemini Interactions and Bedrock (Anthropic and Amazon Nova models)
  converters flag only `Failed` and `NotRun` as tool errors. Earlier stored events have
  no status; the existing structured-payload helpers read their pending and unknown outcomes.
