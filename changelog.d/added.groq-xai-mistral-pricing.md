- **Groq, xAI and Mistral prices** (`adk-model`): `PricingCatalog::standard()` prices
  Groq's GPT-OSS and Qwen models, xAI's Grok models (with the 200K-token long-context
  tier) and Mistral's current models and `-latest` aliases, so their responses carry
  `UsageMetadata::cost`. Each table cites the vendor's price page and retrieval date;
  `PRICING_EFFECTIVE_DATE` is `2026-10-11`. Models priced only on request stay unpriced.
- **Gemini 3.8 Flash and DeepSeek peak/off-peak prices** (`adk-model`): `gemini-3.8-flash`,
  which the API reports for `gemini-3.7-flash` requests, and `gemini-3.1-pro-preview-customtools`
  are priced. DeepSeek rates follow the current price page — `deepseek-flash` $0.30 / $1.20
  with $0.006 cache hits, `deepseek-v4-pro` $1.32 / $3.96 with $0.044 cache hits — at half
  price outside the published UTC peak hours (`OffPeak`, `ModelPrice::cost_usd_at`).
  `deepseek-chat` is no longer priced.
