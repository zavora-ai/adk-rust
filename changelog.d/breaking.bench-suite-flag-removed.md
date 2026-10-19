- **`cargo adk bench --suite` and `BenchConfig::suite` are removed** (`cargo-adk`, `adk-bench`):
  the flag was parsed into `BenchConfig::suite`, which nothing read, so `--suite tau2` or
  `--suite bfcl` ran the ordinary workloads. The `tau2` and `bfcl` adapters do not yet run a
  model, so the flag, the field, and `adk_bench::TaskSuite` are gone rather than wired to them.
