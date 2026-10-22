- **Harness web search defaults to the model provider's built-in search** (`adk-harness`,
  `adk-cli`, spec 3.1 §7.20, I49): a harness with `web(..)` declares the search built-in of
  the provider the model's client serves — Gemini `google_search`, Anthropic `web_search`,
  OpenAI Responses `web_search`, or DeepSeek's Responses `web_search` for `deepseek-v4-pro`
  — with the web allowlist forwarded where the provider takes one, instead of no search.
  It is still declared only to runs that start in Ask or Auto, and a provider that refuses
  a request for it (no search quota, no permission) gets the request again without it and
  the harness carries on without it. A configured
  `search_provider(..)` replaces it with the governed `web_search`;
  `ProviderWebTools::off()` declares none. The provider is read from the client through
  `Llm::native_web_search()`, not from the model name, so a model served through another
  client (a compatible gateway, Ollama, Bedrock) gets no provider tool. After each response that carries a provider search,
  `harness.web.searched` records the queries and cited URLs with `source: "provider"`.
  `HarnessInfo::web_search` reports the path; `adk-rust` prints it on the `Search` banner
  line and under `/web`, and takes `--search provider|off|searxng|deepseek` and a
  `[web]` table (`search`, `searxng_url`) in `harness.toml`; a project file may only set
  `search = "off"`.
