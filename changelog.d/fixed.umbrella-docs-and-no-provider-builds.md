- `cargo test -p adk-rust --doc` passes under every feature set: the crate docs' artifacts example
  is guarded by the `artifacts` feature instead of failing to compile below `standard`. `cargo
  clippy -p adk-rust --no-default-features --features agents,sessions,runner` (no model provider)
  and `cargo clippy -p adk-harness --no-default-features --features auth` pass; both sets, and the
  umbrella doctests, run in the `feature-coverage` PR shard (`scripts/feature-coverage-pairs.txt`
  takes an optional `no-default`/`doc` flags column).
