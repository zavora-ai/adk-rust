- **Prices and limits for current models** (`adk-model`, `adk-anthropic`, `adk-gemini`,
  `adk-eval`): `ModelPricing::for_model_id("claude-fable-5-2")` returned Claude Fable 5's rates
  ($10 / $50, cache reads $1); it and `claude-mythos-5-2` resolve to `FABLE_52` / `MYTHOS_52`
  ($8 / $40, cache reads $0.20), with `FABLE_52_FAST` / `MYTHOS_52_FAST` ($16 / $80) for fast
  mode, and `Model::claude_fable_5_2()`, `claude_mythos_5_2()`, and `claude_mythos_5_1()` return
  their IDs. `PricingCatalog::standard()` prices Claude Fable/Mythos 5.2, GPT-6, Gemini 4
  (Carbon and Argon rates double on 2026-12-01), Mistral Large 4 (`mistral-large-latest` moves
  from $0.50 / $1.50 to $0.70 / $2.10), and Muse Ember (long-context rate over 200K tokens);
  applies the GPT-5.6 and GPT-6 long-context rates above 272K input tokens (previously from
  272K, and not at all for GPT-5.6); records max output for every Claude model and context
  windows for the new models, Claude 4.x, GPT-5.x Pro and Cyber, Grok Build, Muse, and
  `gpt-oss-safeguard-20b`; and bills Groq `gpt-oss-safeguard-20b` cache reads at $0.037.
  `adk_gemini::pricing` adds Gemini 3.8 Flash and Gemini 4, and the Google Search fee covers
  `gemini-4*`. `adk_eval::pricing::default_pricing` bills `deepseek-flash` at $0.30 / $1.20 (was
  $0.44 / $1.32) and adds the current Claude, GPT-6, and Gemini models.
