- **`PathAllowList` denies a call that omits a path argument** (`adk-guardrail`): a call
  without one of the configured arguments was allowed unchecked, so a tool reached under
  another argument name escaped the allow list. It is now denied; `allow_missing()`
  restores the old behaviour for guardrails that name arguments only some tools take.
  Without `on_tools` the guardrail applies to every tool, so a tool that takes no path is
  denied too, and a tool not named in `on_tools` is not checked at all.
- **`ContentFilter::new` and `ContentFilter::blocked_keywords` return `Result`**
  (`adk-guardrail`): a keyword list that could not compile — one past the regex size
  limit — produced a filter that blocked nothing. Both now return
  `GuardrailError::Regex` instead. The built-in presets are unchanged.
