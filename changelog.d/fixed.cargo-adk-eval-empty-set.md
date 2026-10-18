- **`cargo adk eval` refuses an eval set without cases** (`cargo-adk`): a `.test.json` file
  whose `eval_cases` is empty exits non-zero before any case runs instead of reporting
  success for zero cases.
