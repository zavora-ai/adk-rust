- **Cost for every provider** (`adk-model`): every provider sets
  `LlmResponse::provider` and `model` and fills `UsageMetadata::cost` from the
  new `pricing::PricingCatalog` when the provider reports no cost. The catalog
  consolidates the Gemini, OpenAI and Anthropic pricing modules, adds DeepSeek,
  bills cached prompt tokens once, splits Anthropic cache writes by TTL, applies
  long-context tiers, resolves dated aliases and Bedrock/Vertex identifiers, and
  carries its verification date (`PRICING_EFFECTIVE_DATE`). Unpriced models
  keep `cost: None`. `usage_tracking::with_priced_usage_tracking` exposes the
  shared path.
