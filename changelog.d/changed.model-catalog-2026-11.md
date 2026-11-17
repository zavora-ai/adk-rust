- **Provider defaults move to current models** (`adk-model`, `adk-gemini`, `cargo-adk`):
  `GEMINI_DEFAULT` is `gemini-3.8-flash` (Google routes the deprecated `gemini-3.7-flash` to it),
  `OPENAI_DEFAULT` `gpt-6.1-sol`, `ANTHROPIC_DEFAULT` `claude-sonnet-5-5`, `XAI_DEFAULT`
  `grok-4.7`, `MISTRAL_DEFAULT` `mistral-large-latest` (Mistral Large 4), `FIREWORKS_DEFAULT`
  `accounts/fireworks/models/kimi-k3`, `TOGETHER_DEFAULT` `moonshotai/Kimi-K3.1-Pro`,
  `MINIMAX_DEFAULT` `MiniMax-M3`, and `ZHIPU_DEFAULT` `glm-5.3`. `adk_gemini::Model::default()`
  is `Model::gemini_3_8_flash()`, the Bedrock default is `us.anthropic.claude-sonnet-5-5`, and
  `cargo adk` templates, the harness agent picker, the Gemini speech-to-text provider, and the
  `adk-eval` judge use the new defaults.
- **Model catalog refreshed for 2026-11-17** (`adk-model`): `MODEL_CATALOG` adds Claude Fable
  5.2 and Mythos 5.2, GPT-6 (`gpt-6-astra`, `gpt-6.1-sol`, `gpt-6.1-terra`, `gpt-6-sol`,
  `gpt-6-luna`), Gemini 4 (`gemini-4-hydrogen`, `gemini-4-carbon`, `gemini-4-argon`),
  `gemini-nano-banana-2.1`, Mistral Large 4, Muse Ember, and current xAI, Groq, MiniMax, and
  Zhipu models; `"muse"` joins `KNOWN_PROVIDERS`. Deprecations with their shutdown dates and
  replacements cover Gemini 3.7/3.5 Flash, Gemini 2.5 Pro/Flash, GPT-5/5-mini/5-nano/5-pro, o3,
  GPT-4.1/4o, `claude-sonnet-4-5`, Mistral Medium 3.5 and Large 3, `deepseek-v4-flash`, and
  Fireworks Kimi K2.6; `o4-mini`, `o3-mini`, `o1`, `gpt-4.1-nano`, Claude Opus 4.1/4, Sonnet 4,
  the Claude 3 models, and Groq's Llama 4 Scout and Qwen3 32B are retired, so
  `warn_if_obsolete` logs them and `validate_model_selection` rejects the retired ones.
