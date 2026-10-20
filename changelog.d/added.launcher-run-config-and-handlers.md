- **`Launcher` run configuration, identity and handlers** (`adk-cli`):
  `with_run_config(RunConfig)` configures console runs and merges with
  `with_streaming_mode`, which no longer replaces the whole configuration;
  `with_tool_policy` and `with_confirmation_handler` take precedence over the same fields of
  the run configuration. `with_user_id` replaces the fixed `"user"`, `with_session_id`
  resumes a session or creates it under that id, and `with_console_command` registers a
  `ConsoleCommand` slash command that `/help` lists. For serve mode,
  `with_approval_service` mounts `/api/approvals` with its expiry ticker and
  `with_background_runs` mounts `/api/runs`. `build_app` now builds through `ServerBuilder`
  (same routes as before) and returns a configuration error while a tool policy is set,
  since the server would run without it. `adk-cli` enables the `approvals` and `background`
  features of `adk-server`.
