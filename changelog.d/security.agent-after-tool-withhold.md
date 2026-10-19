- **A failed after-tool stage no longer turns a completed call into an error** (`adk-agent`):
  when an `after_tool_callback`, an `after_tool_callback_full`, or an enhanced plugin's
  `after_tool_call` returned an error, `LlmAgent` replaced the tool's result with
  `{"error": ...}`, so a model that retried under a new call ID repeated a side effect that had
  already happened — a second charge. The call is now answered with
  `adk_core::result_withheld_response` (`"status": "result_withheld"`), which states that the
  tool ran and must not be called again; the unscreened result stays withheld, and the
  remaining after-tool stages do not run.
