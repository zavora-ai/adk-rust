- **A guarded tool whose audit sink fails is refused** (`adk-auth`): `ProtectedTool`,
  `ProtectedToolDyn`, `ScopedTool`, and `ScopedToolDyn` default to `AuditFailureMode::Block`,
  so a sink error fails the call with `auth.audit_failed` (`Unavailable`) instead of being
  ignored. Call `with_audit_failure_mode(AuditFailureMode::Warn)` on `ProtectedTool`,
  `ProtectedToolDyn`, `AuthMiddleware`, or `ScopeGuard` to log the failure and keep running.
