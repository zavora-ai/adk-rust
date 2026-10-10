- **Audit-write failures block guarded tools** (`adk-auth`): `ProtectedTool`,
  `ProtectedToolDyn`, `ScopedTool`, and `ScopedToolDyn` discarded the result of
  `AuditSink::log`, so a tool ran with no record of the access decision. A failed write
  now refuses the call with `auth.audit_failed`. `AuditFailureMode::Warn`, set with
  `with_audit_failure_mode` on the wrappers, `ScopeGuard`, and `AuthMiddleware`, logs the
  failure and continues instead.
- **The file audit chain survives restarts and can be verified** (`adk-auth`):
  `FileAuditSink::with_chaining` restarted its chain on every open, so the first event
  after a restart linked to nothing. It now resumes from the file's last line, and a
  failed write no longer advances the chain. `FileAuditSink::verify` and `verify_file`
  report the first line that was edited, inserted, or removed, and
  `with_hmac_chaining` keys the chain with HMAC-SHA256 so it cannot be recomputed without
  the key.
