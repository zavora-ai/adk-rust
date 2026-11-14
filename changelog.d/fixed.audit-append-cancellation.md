- **A dropped audit append no longer strands the writer** (`adk-auth`): `AuditLog` updated its
  record count only after the store replied, so a caller dropped mid-write (a client hanging up
  on a request handler) left records in the store that the writer did not count, and every later
  append failed with `audit_log.seq_conflict` ("Another writer is using this log_id"). The store
  write and the count now finish together in one task, whether or not the caller waits.
