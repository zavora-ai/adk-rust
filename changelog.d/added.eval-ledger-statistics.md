- **Ledger statistics and `cargo adk eval --ledger`** (`adk-eval`, `cargo-adk`): the
  `adk_eval::ledger` module computes, as pure functions with no I/O and no model, the
  rolling 28-day median and MAD of each dimension of a content-free activity ledger per
  weekday and weekend class, the 14-day warm-up, and the deviation test of the last 7 and
  14 days, `|recent − baseline| > max(2.5 × MAD, floor)` with a floor per dimension;
  `is_meaningful` (two dimensions, or one for 10 days) and `DriftState` (runs and the 7-day
  cooldown per dimension) as pure state transitions; and `LedgerRow`, `parse_jsonl` and
  `daily_series` for the camelCase JSONL export. `cargo adk eval --ledger <export.jsonl>`
  runs them over an exported ledger as a CI regression gate: a table or JSON report
  (`--format json`), `--floor DIMENSION=VALUE`, and a non-zero exit on a meaningful
  deviation or on an export too short to finish the warm-up.
