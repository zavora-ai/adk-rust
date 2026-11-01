- **Audit records without subject ids** (`adk-core`, `adk-auth`, `adk-session`):
  `subject::PseudonymousRecorder` names visitors, or every subject, by registry pseudonym in
  new audit records; `AuditLogStore::redact` drops chosen record bodies and keeps their hashes
  on every backend, so checkpoints and proofs still verify; `subject::AuditLogSubjectData`
  redacts the records that name a subject being erased; and `adk_auth::PseudonymousAuditSink`
  pseudonymises `AuditEvent` users and drops client addresses.
