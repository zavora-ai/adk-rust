- **Audit-write failures block guarded tools** (`adk-auth`): `ProtectedTool`,
  `ProtectedToolDyn`, `ScopedTool`, and `ScopedToolDyn` discarded the result of
  `AuditSink::log`, so a tool ran with no record of the access decision. A failed write
  now refuses the call.
- **The file audit chain survives restarts and can be verified** (`adk-auth`):
  `FileAuditSink::with_chaining` restarted its chain on every open, so the first event
  after a restart linked to nothing. It now resumes from the file's last line, and a
  failed write no longer advances the chain. `FileAuditSink::verify` and `verify_file`
  name the first line whose link does not match, which finds an edited or inserted line
  and a removed one before the last, and `with_hmac_chaining` keys the chain with
  HMAC-SHA256 so it cannot be recomputed without the key.
