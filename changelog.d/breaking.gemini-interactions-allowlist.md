- **The Gemini Interactions allowlist names current models** (`adk-model`):
  `InteractionTarget::parse` and `GeminiModel::use_interactions_api(true)` accepted
  `gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite` and the deprecated
  `gemini-3-flash-preview`; the endpoint answers the 2.5 models with 404 for new projects.
  They are removed from `MODEL_TARGETS` and now fail at configuration with
  `model.gemini.interactions.unsupported_target`; `gemini-3.8-flash` is added.
