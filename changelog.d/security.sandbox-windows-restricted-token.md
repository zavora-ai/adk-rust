- **Windows restricted-token enforcer** (`adk-sandbox`, feature `sandbox-windows`): the
  Windows enforcer is now a restricted-token-plus-ACLs tier (spec 3.2 §10 W5) instead of an
  unimplemented AppContainer stub. `WindowsEnforcer::run_confined` runs a command under a
  restricted token: privileges are removed, the user SID and widening groups are deny-only,
  public runtime groups stay enabled, and `WRITE_RESTRICTED` applies `RESTRICTED` and the
  logon SID to writes. Mode-limited grants for those two SIDs admit the policy's paths, and
  a Job Object kills every descendant with the call. Owner-only private reads need a policy
  grant; enabled public groups still admit public files and system runtime dependencies.
  Writes require matching restricted/logon access, so ordinary `Everyone` or `Users` write
  grants do not authorize external writes. ACL grants retain their existing persistent
  lifetime. Network access and the integrity level are not restricted.
  The enforcer's name is now `restricted-token`; `probe` and `configure_command` report that
  the generic `sandboxed_command` path cannot carry a restricted token, so a sandboxed shell
  built through that path stays degraded on Windows.
