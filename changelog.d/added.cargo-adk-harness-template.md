- **`harness` template** (`cargo-adk`, spec 3.1 §8.3): `cargo adk new NAME --template
  harness` scaffolds a harness project — `HarnessAgent` over SQLite sessions in
  `.adk/state.db`, `HarnessLauncher` with console approvals (`cargo run` is the REPL,
  `cargo run -- serve` serves it on loopback), an `AGENTS.md` skeleton, an example skill in
  `.skills/example/SKILL.md`, `.adk/harness.toml`, and `tests/harness_smoke.rs`, a scripted
  test that Plan mode refuses a write. `adk-harness` gains `HarnessConfig`
  (`HarnessConfig::discover`, `HarnessConfig::from_toml`), `HARNESS_CONFIG_FILE` and
  `HarnessAgentBuilder::config`, which applies the project file's mode, sandbox and
  `fail_if_unavailable` only where they tighten.
