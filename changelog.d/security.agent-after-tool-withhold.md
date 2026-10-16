- **A failed after-tool stage no longer turns a completed call into an error** (`adk-agent`):
  when a runner plugin `after_tool` hook, an `after_tool_callback`, an
  `after_tool_callback_full`, or an enhanced plugin's `after_tool_call` returned an error,
  `LlmAgent` replaced the tool's result with `{"error": ...}`. The side effect had already
  happened and the action ledger recorded success, so a model that retried under a new call ID
  repeated it — a second charge. The call is now answered with a `result_withheld` response
  (`adk_core::result_withheld_response`) that states the tool ran and must not be called
  again; the unscreened result stays withheld, the remaining after-tool stages do not run,
  and the ledger record stays completed.
