- **Hooks on tool events in `adk-harness`** (feature `hooks`, spec 3.1 §7.22): `[[hooks]]`
  tables in the workspace's `.adk/harness.toml`, the user's `harness.toml` and
  `HarnessAgentBuilder::hooks` run on `pre_tool`, `post_tool`, `post_edit`, `on_hold`,
  `on_stop` and `on_session_start`, through the same `CommandLauncher` as `bash` — inside
  the OS sandbox when the shell is — with the event as JSON on stdin and the outcome on
  stdout.
  - A hook can deny a call (inside the policy decision, before standing rules), fail or
    annotate a result, or notify the user; it can never allow a call, change the mode,
    create a rule or answer a call: `PreToolVerdict` has no `Allow`, `{"decision":"allow"}`
    is `continue` and audited as `ignored_grant`, and a hook's text reaches the model only
    inside a delimited `<hook-note>` block.
  - A hook that fails — a non-zero exit, a crash, a timeout, a refused launch, an audit
    record that cannot be written — applies its `on_failure` (`HookFailure`): a `pre_tool`
    hook blocks the call by default, `on_failure = "continue"` opts it out, and
    `on_failure = "block"` makes a `post_tool` or `post_edit` failure fail the result;
    `max_per_event` and `max_time_per_turn` bound a turn; every run is recorded as
    `harness.hook.ran` with the command digest, source, isolation, exit code, duration and
    outcome.
  - `HarnessAgentBuilder::without_hooks` runs none; `HarnessRuntime::hooks` returns the
    merged set. `examples/harness_agent` gains `hooks_cannot_grant` (spec 3.1 §5 H13) and
    the live `live_screenshot_and_format_hook`.
