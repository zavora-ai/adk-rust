- **`bash` returns at most 30,000 characters of output** (`adk-devtools`, `adk-harness`):
  a longer output keeps its first 10,000 and last 20,000 characters around a marker naming
  the bytes left out, instead of up to 1 MiB per stream from the start only. An
  `OutputStore` keeps the whole output, named by the result's `output_id` and read with
  `shell_output`; `Workspace::max_output_bytes` now bounds that file and defaults to 64 MiB
  (spec 3.1 §7.33 fix 8).
