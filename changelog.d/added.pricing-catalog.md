- **Cost for every provider** (`adk-model`): every provider sets `LlmResponse::provider` and
  `model` and fills `UsageMetadata::cost` from `pricing::PricingCatalog` when the provider
  reports no cost. Unpriced models keep `cost: None`; local Ollama models cost zero.
  `usage_tracking::with_priced_usage_tracking` exposes the shared path, and
  `PricingCatalog::new` with `with_price` builds a table for custom models.
  - `PricingCatalog::standard()` holds list prices for Gemini, OpenAI and Anthropic (the
    rates of the existing `adk_gemini::pricing`, `adk_model::openai::pricing` and
    `adk_anthropic::pricing` modules), DeepSeek, Groq's GPT-OSS and Qwen models, xAI Grok
    and Mistral, verified on `PRICING_EFFECTIVE_DATE` (`2026-10-11`). Models a vendor prices
    only on request stay unpriced, as do DeepSeek's unlisted `deepseek-chat` and
    `deepseek-reasoner`.
  - The catalog bills cached prompt tokens once, splits Anthropic cache writes by TTL,
    applies long-context tiers (xAI's from 200K tokens), and applies DeepSeek's half-price
    rates outside its published UTC peak hours (`OffPeak`, `ModelPrice::cost_usd_at`).
  - Lookup resolves dated and `-latest` aliases and Bedrock and Vertex AI identifiers.
    `gemini-3.8-flash`, which the API reports for `gemini-3.7-flash` requests, is priced.
