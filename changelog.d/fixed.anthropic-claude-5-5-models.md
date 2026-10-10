- **Claude 5.5 and Fable 5.1 pricing** (`adk-anthropic`): `ModelPricing::for_model_id`
  resolves `claude-opus-5-5` to Claude Opus 5.5 rates ($4 / $20 per MTok) instead of
  Claude Opus 5, `claude-sonnet-5-5` to Claude Sonnet 5.5 (cache reads $0.10 per MTok),
  and `claude-fable-5-1` and `claude-mythos-5-1` to their $0.25 per MTok cache-read rate.
  `claude-haiku-5-5` resolves to `ModelPricing::HAIKU_55`, the rate for prompts of
  100,000 tokens or fewer, instead of `None`. New constants `OPUS_55`, `SONNET_55`,
  `HAIKU_55`, `HAIKU_55_LONG_PROMPT`, `FABLE_51`, `MYTHOS_51`, and `OPUS_55_FAST`
  ($8 / $40 per MTok) carry the published rates, and `ModelPricing` implements
  `PartialEq`.

- **Claude Haiku 5.5 sampling and budget thinking** (`adk-model`): `AnthropicClient` rejects
  `temperature`, `top_p`, `top_k`, and `ThinkingMode::Enabled` for `claude-haiku-5-5` before
  sending the request, with the same error codes as the other Claude 5 models, instead of
  forwarding a request the API answers with a 400.
