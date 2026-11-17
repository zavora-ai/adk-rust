- **A broken authorization hook or callback blocks the call** (`adk-harness`, `adk-core`,
  `adk-agent`, `adk-plugin`): a `pre_tool` hook that exits non-zero, crashes, times out or
  cannot be launched denies the call with the failure as the reason, instead of letting it
  through; `on_failure = "continue"` (`HookFailure::Continue`) opts an advisory hook out, as
  the Claude Agent SDK's `on_failure` does. `HookSpec::fail_closed` is replaced by
  `HookSpec::on_failure`. A before-tool callback or enhanced `before_tool_call` plugin that
  panics now answers the call `not run` with `callback.panicked` (`adk_core::guard_callback`)
  in `LlmAgent`, `CodeActAgent` and `EnhancedPluginManager`, where it unwound through the
  agent before.
