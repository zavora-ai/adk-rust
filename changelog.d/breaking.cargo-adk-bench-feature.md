- **`cargo adk bench` needs the `bench` feature** (`cargo-adk`): the subcommand and
  `adk-bench` are behind a `bench` feature that is off by default, which takes 37 packages
  off `cargo install cargo-adk`. Without the feature, `cargo adk bench` exits with status 2
  and the install command that adds it. Install with
  `cargo install cargo-adk --features bench` to benchmark.
