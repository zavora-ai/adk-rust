- **A scaffolded harness or autonomous agent starts without warnings** (`cargo-adk`,
  `adk-sandbox`, `adk-cli`): the `harness` and `autonomous` templates initialize ADK telemetry
  with `init_with_adk_exporter` instead of installing a `tracing_subscriber`, so `up` and
  `serve` reuse the exporter and the web home's developer panel records each run's spans; the
  generated projects no longer depend on `tracing-subscriber`. The sandbox enforcers log a
  symbolic link resolved in an allowed path — `/tmp`, a Homebrew `opt` link, the Cryptexes on
  macOS's `PATH` — at `debug`, and warn only when a read-only path inside a writable one is
  reached through a link a command can replace. The launcher logs at `debug` when the process
  already has a tracing subscriber of its own, which stays.
