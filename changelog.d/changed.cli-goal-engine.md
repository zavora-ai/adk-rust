- **`adk-rust goal` runs the harness goal engine** (`adk-cli`, spec 3.1 §7.11, §8.1): the
  goal starts with `Harness::start_goal` and runs as a durable run in `harness.db` on a
  worker inside the command, instead of a loop of turns in the CLI. A call held without a
  terminal parks the run; after `adk-rust approvals approve`, `adk-rust goal --resume`
  continues it at the held call, in the same session and iteration; after Ctrl-C or a crash
  it reclaims the run once its 10-second lease lapses. The `--until` check runs
  through the harness shell for up to `--check-timeout` seconds (600 by default). The
  check summary in the conversation reads "Goal check `CMD` failed (exit N) on iteration I
  of M". `HarnessLauncher` gains `with_run_store`, `run_goal`, `resume_goal` and `goal`, and
  `adk-rust serve` serves the `harness.goal` durable-run target.
