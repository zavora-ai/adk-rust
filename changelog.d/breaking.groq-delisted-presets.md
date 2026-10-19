- **Groq presets for delisted models are removed** (`adk-model`): `GroqConfig::llama8b`,
  `GroqConfig::mixtral`, `GroqConfig::gemma`, `GroqClient::llama8b` and `GroqClient::mixtral`
  selected `llama-3.1-8b-instant`, `mixtral-8x7b-32768` and `gemma2-9b-it`, which Groq no longer
  serves, so every request failed with `model_not_found`. Use `GroqConfig::gpt_oss_20b` (new)
  for the fast, lower-cost model, `GroqConfig::gpt_oss_120b` for the default, or
  `GroqConfig::new(key, model)` for any other model Groq lists.
