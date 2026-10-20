- **`sandbox::sandboxed_command`** (`adk-sandbox`): builds a `tokio::process::Command` that
  runs a program under a `SandboxEnforcer` and `SandboxPolicy`, starting in a working
  directory the caller names — the user's workspace for a harness shell, instead of the
  per-execution scratch directory `ProcessBackend` uses. The environment is cleared and set
  to `policy.env`; the enforcer's wrapper is resolved on the caller's `PATH`; on macOS an
  Xcode shim is replaced by the tool behind it. `ProcessBackend` builds its sandboxed
  commands through it, so program resolution and the Xcode shim handling exist once.
  Spec 3.1 §10 S1.
