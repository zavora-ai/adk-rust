- **Contained processes on Windows** (`adk-sandbox`, `adk-devtools`, spec 3.2 §10 W5):
  `WindowsEnforcer::spawn_contained` starts a command under the restricted token and returns
  at once with an `adk_sandbox::process_tree::ContainedChild` — the process (`wait`,
  `try_wait`), its Job Object, and its standard streams as pipes, the only handles it
  inherits. `WindowsEnforcer::probe_confined` probes that path; the generic `probe` still
  reports the `configure_command` path unavailable. `CommandLauncher::spawn_contained`
  (Windows) lets a launcher start the process itself, which `BashTool` then runs as it runs a
  command it spawns.
