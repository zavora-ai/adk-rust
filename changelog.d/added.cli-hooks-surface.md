- **Hooks in the `adk-rust` binary** (`adk-cli`, spec 3.1 §7.22, §8.1): the `harness`
  feature now builds `adk-harness` with `hooks`, so `[[hooks]]` tables in the workspace's
  `.adk/harness.toml` and in the user config (`<config dir>/adk-rust/harness.toml`) run —
  the user's before the workspace's, the workspace's only under its trust. `--no-hooks` runs
  none; `/hooks` lists the effective set with each hook's source, isolation, timeout and
  whether it runs; the trust prompt lists each workspace hook with its command; a note
  from an `on_hold` or `on_stop` hook is shown as `[hook:source] text`; and a `[[hooks]]`
  table is no longer reported as an unknown key.
