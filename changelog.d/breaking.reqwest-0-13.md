- **reqwest 0.13 across the workspace, one rustls crypto provider** (`adk-core`, `adk-auth`,
  `adk-tool`, `adk-model`, `adk-gemini`, `adk-gcp`, `adk-server`, `adk-site`, `adk-anthropic`
  and every crate that takes or returns `reqwest` types): APIs that accept or return
  `reqwest::Client`, `RequestBuilder`, `HeaderMap` or `Error` now use reqwest 0.13, so an
  application passing its own client depends on `reqwest = "0.13"`. TLS is rustls on
  `aws-lc-rs` with the platform verifier (the operating system's trust store); `ring` is no
  longer compiled for HTTP, the `reqwest-openai` and `reqwest-mcp` aliases are gone, and
  `adk-browser` moves to `thirtyfour` 0.38.
