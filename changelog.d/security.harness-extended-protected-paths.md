- **Every harness protects its judge, CI and build configuration** (`adk-harness`): from
  the workspace root, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `CODEOWNERS`,
  `publish.sh`, `.github/**`, `scripts/**`, `xtask/**`, `adk-eval/**`, the governance
  sources of `adk-core/src`, the harness's own policy and approval sources,
  `adk-harness/tests/suite/**` and `.adk/selfimprove/**` are protected paths: an edit needs
  a one-shot approval in Ask and Auto, is denied in ReadOnly and Plan, and no standing rule
  covers it (spec 3.4 §7.5, §10 P1; spec 3.1 §9.2 I29).
