- Two tests no longer fail at random in CI: the `adk-sandbox` environment-isolation property no
  longer generates variables a POSIX shell sets at startup (`IFS`, `PS1`, `OPTIND`, …), and the
  `adk-harness` test helper waits 90 s instead of 20 s for a run, which the live browser test needs
  on a shared runner.
- `adk-harness` builds with `mcp` and without `web` (resource digests no longer call into the
  `web` module), and the `adk-model` timeout tests build with only the default `gemini` feature;
  `scripts/feature-coverage-pairs.txt` now covers both combinations.
