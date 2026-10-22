- Two tests no longer fail at random in CI: the `adk-sandbox` environment-isolation property no
  longer generates variables a POSIX shell sets at startup (`IFS`, `PS1`, `OPTIND`, …), and the
  `adk-harness` test helper waits 90 s instead of 20 s for a run, which the live browser test needs
  on a shared runner.
