- **`FinishReason` is `#[non_exhaustive]` and gains `Refusal` and `MalformedToolCall`**
  (`adk-core`, `adk-model`): a `match` on `FinishReason` outside `adk-core` needs a wildcard
  arm. A model that declined finishes with `Refusal` — Anthropic `stop_reason: "refusal"`
  (was `Safety`), an OpenAI Chat Completions `refusal` (was `Safety`), a Responses `refusal`
  content part (was `Stop`), and an OpenRouter `refusal` or `native_finish_reason` of `refusal`.
  A tool call the provider reports as invalid finishes with `MalformedToolCall` — Gemini
  `MALFORMED_FUNCTION_CALL` and `UNEXPECTED_TOOL_CALL` (was `Other`), Bedrock
  `malformed_tool_use` (was `Other`). `Safety` stays the content-filter block. Bedrock
  `model_context_window_exceeded` finishes with `MaxTokens` (was `Other`). The new variants
  serialize as `"Refusal"` and `"MalformedToolCall"`; stored events keep loading, and a name
  this version does not know loads as `Other`. `adk_gemini::FinishReason` is
  `#[non_exhaustive]` too.
- **A refusal or a malformed tool call ends the run with an error** (`adk-agent`): `LlmAgent`
  and `CodeActAgent` emit the model's event, so the session keeps the refusal text, and then
  fail the run with `model.refusal` (`ErrorCategory::Forbidden`) or
  `model.malformed_tool_call` (`ErrorCategory::Internal`), with or without a provider
  `error_code`; no tool call of that turn runs. An OpenAI-style refusal was the run's
  successful answer, so `adk-rust code` and `adk-rust goal` now exit `1` on one. A summarizer
  model that declines fails compaction with `model.summary_incomplete` instead of replacing
  history with the refusal.

  Migration: add a wildcard arm to `match`es on `FinishReason`. A caller that showed a
  refusal as the answer reads it from the run's last model event, or from the `model.refusal`
  error's message.
