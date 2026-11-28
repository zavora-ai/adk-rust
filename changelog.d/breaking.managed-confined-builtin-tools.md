- **Managed built-in tools run confined, and the `sandbox` feature is removed** (`adk-managed`):
  `bash` and `code_execution` run under the platform's OS sandbox (Seatbelt on macOS,
  bubblewrap on Linux) in a workspace of the session's own, and are refused with
  `managed.builtin.sandbox_unavailable` where none runs, Windows included. `filesystem`
  resolves every path inside that workspace and refuses one that escapes it, symbolic links
  included; `read` and `write` return `adk-devtools`' `read_file` and `write_file` results.
  To run the commands on the host as before, pass
  `with_builtin_tools(BuiltinTools::unsafe_unconfined_host())`. `adk-sandbox` now always links,
  so drop `sandbox` from the dependency's features; `with_sandbox` needs no feature.
  `build_agent(def, model, sandbox)` becomes `build_agent(def, model, &BuiltinTools::default())`,
  `ManagedBuiltinTool::new` takes a `BuiltinTools`, and `DefaultManagedAgentRuntime::sandbox()`
  becomes `builtin_tools()`.
