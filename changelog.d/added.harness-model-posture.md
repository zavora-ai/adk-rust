- **Model posture** (`adk-harness`, `adk-core`, `adk-model`, `adk-gemini`; spec 3.1 §7.23):
  `EffortPolicy` names a reasoning `Effort` (`Low`, `Medium`, `High`) for every mode and
  for the member, distiller and sampling models (defaults: plan high, members low);
  `HarnessAgentBuilder::effort` sets it and `HarnessAgent::effort` reads it. Each request
  carries its level as `GenerateContentConfig::effort` (§10 C4), which the providers map
  through the new `From<adk_core::Effort>` conversions for Anthropic `Effort`, OpenAI
  `ReasoningEffort` and Gemini `ThinkingLevel`. `cache_capable(model)` gives the runner
  the default explicit context cache for a Gemini model and `without_context_caching()`
  turns it off. `harness.spend` carries `promptTokens`, `cachedTokens` and `effort`, and
  `SpendReport` lists cached against uncached prompt tokens per turn for `/cost`.
