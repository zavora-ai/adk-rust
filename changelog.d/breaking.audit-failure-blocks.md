- **A guarded tool whose audit sink fails is refused** (`adk-auth`): `ProtectedTool`,
  `ScopedTool`, and the guards that build them default to `AuditFailureMode::Block`, so a
  sink error fails the call with `auth.audit_failed` instead of being ignored. Call
  `with_audit_failure_mode(AuditFailureMode::Warn)` to keep running when the sink is down.
