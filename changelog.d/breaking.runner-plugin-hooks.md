- **Runner plugin model, tool, and agent callbacks run** (`adk-core`, `adk-plugin`,
  `adk-runner`, `adk-agent`): a `PluginManager` on the runner had only its run,
  user-message, and event callbacks called, so a plugin `before_tool` that denied a call
  or a `before_model` that replaced one did nothing. The runner now adds the manager to
  the new `RunConfig::invocation_hooks`, and `LlmAgent` and `CodeActAgent` run its
  `before_agent`, `after_agent`, `before_model`, `after_model`, `before_tool`,
  `after_tool`, and `on_tool_error` callbacks ahead of their own, for every agent the run
  reaches. A plugin callback that returns a value short-circuits the agent's callbacks of
  that kind. `on_model_error` has no caller; `PluginManager` logs a warning when a plugin
  sets it. A `RunConfig` struct literal without `..Default::default()` must add
  `invocation_hooks`.
- **`adk_runner::Callbacks` removed** (`adk-runner`): the type and the
  `BeforeModelCallback`, `AfterModelCallback`, `BeforeToolCallback`, and
  `AfterToolCallback` aliases it exported were never called by the runtime. Register
  callbacks on the agent builder, or as runner plugins.
