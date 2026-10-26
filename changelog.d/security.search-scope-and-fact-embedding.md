- **The fetch allowlist no longer filters search, and facts stay with the chat provider or
  on this machine** (`adk-harness`, `adk-cli`, spec 3.1 §7.32, I73): `WebConfig::allowed_domains`
  governs `web_fetch` and the browser pack only and is never forwarded to a search; the
  search filter is the new `WebConfig::search_domains` (`SearchDomains::Allowed` or
  `Blocked`), forwarded to Anthropic and OpenAI search, applied to the governed
  `web_search`'s results, and named as not applied for Gemini and DeepSeek. `adk-rust` reads
  it from `[web] search_domains`, which `.adk/harness.toml` may only narrow. Long-term facts
  and the recall query, which carries every user message, are embedded by the chat
  provider's own embedder (Gemini, OpenAI) or the local ONNX model, never by another
  provider whose key happens to be set; `--embedder local|gemini|openai|lexical` and
  `[memory] embedder` choose otherwise, and every session's banner names the embedder.
  Stored vectors carry their embedder's id (`FactEmbedding`,
  `adk_memory::EmbeddingProvider::model_id`), so recall embeds a fact again instead of
  comparing two models' vectors. An agent a person added without search no longer gets
  the provider's search.
