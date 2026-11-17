- **`adk-ui` 2 is retired; `adk-ui` 3 builds on `genui`** (spec 3.3 decision 11, amended in
  #1393): agent-drawn UI is `adk-surface`'s `genui` feature, and `adk-ui` 3 adds extension
  catalogs to it (charts, metrics, pages, scenes) instead of its own render tools and
  protocols. `adk-ui` 2.2 stays on crates.io for ADK-Rust 2;
  `docs/official_docs/surfaces/genui.md` covers extension catalogs and maps each `adk-ui` 2 tool
  to its replacement. The `adk_ui` profile of `adk-server` stays deprecated, with a sunset target
  of 2026-12-31.
