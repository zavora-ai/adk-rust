- **Windows restricted-token enforcer** (`adk-sandbox`, feature `sandbox-windows`): the
  Windows enforcer is now a restricted-token-plus-ACLs tier (spec 3.2 §10 W5) instead of an
  unimplemented AppContainer stub. `WindowsEnforcer::run_confined` runs a command under a
  restricted token (privileges removed, `BUILTIN\Users` and Authenticated Users deny-only,
  restricting SIDs `Everyone`, `RESTRICTED` and the logon SID), grants `RESTRICTED` access
  to the policy's paths, and holds the process in a Job Object that kills every descendant
  with the call. Writes outside the granted paths and reads of ungranted paths that are not
  world-readable are refused. Network access and the integrity level are not restricted.
  The enforcer's name is now `restricted-token`; `probe` and `configure_command` report that
  the generic `sandboxed_command` path cannot carry a restricted token, so a sandboxed shell
  built through that path stays degraded on Windows.
