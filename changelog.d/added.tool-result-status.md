- **Tool result status** (`adk-core`, `adk-agent`, `adk-realtime`, `adk-runner`,
  `adk-harness`): an answer the framework writes in place of a tool's result carries an
  `adk_core::ToolResultStatus` — `refused`, `not_run`, `held`, `failed`, `outcome_unknown`,
  `withheld` or `succeeded` — in the function response's annotations, with the code and category
  of the error behind it, so clients, evals and audits read what happened to a call without
  parsing the payload, which is unchanged. `LlmAgent`, `CodeActAgent` (on a resumed held call's
  answer), `RealtimeAgent`, `IntegratedRealtimeRunner`, the runner's answers to cancelled and
  recovered calls, and the harness's ledger answers set it. Adds `TOOL_RESULT_STATUS_KEY`,
  `TOOL_RESULT_ERROR_CODE_KEY`, `TOOL_RESULT_ERROR_CATEGORY_KEY`, `Part::tool_result_status`,
  `Part::tool_result_error_code`, `Part::tool_result_error_category` and
  `Part::set_tool_result_status`. Proposed, with a first implementation, by
  [dux-web](https://github.com/dux-web) (#1006).
