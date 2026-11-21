- **docs.rs pages document every feature-gated API** (`adk-awp`, `adk-site`, `adk-bench`,
  `adk-harness`, `adk-gemini`): docs.rs builds `adk-awp` and `adk-site` with all features, so
  the `signing`, `verify` and `conformance` modules and the `host-*` and `gen-*` adapters
  appear; `adk-bench` adds `bfcl`, `tau2` and `vision-cloud`, and `adk-harness` adds
  `remote-shell`. The `adk-gemini` crate docs no longer list a `studio` feature, which does
  not exist; the AI Studio backend is always compiled.
