- **`SsoAccessControl` refuses a grant its audit sink cannot record** (`adk-auth`):
  `check_token` discarded the error from `AuditSink::log`, so a token was granted access
  with no audit record while `ProtectedTool` and `ScopeGuard` refused the same call. A grant
  whose record fails now returns `SsoError::AuditFailed` under the default
  `AuditFailureMode::Block`; `SsoAccessControlBuilder::audit_failure_mode(AuditFailureMode::Warn)`
  logs the failure and grants access without a record. A denial is returned as
  `SsoError::AccessDenied` whether or not its record was written.
