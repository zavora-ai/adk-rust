- **Groq, xAI and Mistral prices** (`adk-model`): `PricingCatalog::standard()` prices
  Groq's GPT-OSS and Qwen models, xAI's Grok models (with the 200K-token long-context
  tier) and Mistral's current models and `-latest` aliases, so their responses carry
  `UsageMetadata::cost`. Each table cites the vendor's price page and retrieval date;
  `PRICING_EFFECTIVE_DATE` is `2026-10-11`. Models priced only on request stay unpriced.
