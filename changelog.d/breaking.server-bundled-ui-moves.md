- **`adk-server` no longer bundles a runtime UI** (spec 3.3 §10 S1): `/`, `/ui/`,
  `/ui/{*path}` and `/ui/assets/config/runtime-config.json` answer `404`, the `web_ui`
  module and the `rust-embed` dependency are gone, and the crate ships no assets. The REST,
  SSE, A2A and `/api/ui/*` APIs are unchanged. The web home of `adk-surface`'s `web` feature
  replaces it: mount `adk_surface::web::router(engine)` with `ServerBuilder::add_api_routes` and
  `adk_surface::web::home()` with `add_root_routes`, behind a `RequestContextExtractor`.
  `adk_cli::Launcher` serve mode serves the REST API only; `adk-rust serve` and `adk-rust up`
  serve the web home, and `adk-rust serve --agent chat` serves no page. See
  docs/official_docs/surfaces/web-home.md.
