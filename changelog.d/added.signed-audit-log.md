- **Signed, verifiable audit log** (`adk-auth`, `adk-core`, `adk-session`, `adk-cli`):
  `adk_auth::signed_audit::AuditLog` (feature `signed-audit`) appends RFC 8785-canonical
  `AuditRecord`s to a per-replica log linked by `prev_hash`, hashes them into an RFC 9162
  Merkle tree with inclusion and consistency proofs, and signs C2SP tlog checkpoints with
  Ed25519 every N records or T seconds, in the signed-note format of Go's
  `golang.org/x/mod/sumdb/note`. `rotate_signer` records a `key_rotated` entry and
  dual-signs the next checkpoint; `CheckpointAnchor` (with `FileCheckpointAnchor`)
  publishes checkpoints outside the log. `AuditLog` is an `AuditSink`, and
  `Arc<T: AuditSink>` is now one too.
- **`AuditLogStore`** (`adk-core`, `adk-session`): the store trait, `InMemoryAuditLogStore`,
  `SqliteAuditLogStore`, and `PostgresAuditLogStore`, proven by
  `audit_log_store_conformance_tests!`. `append` is a compare-and-swap on the log size
  (`audit_log.seq_conflict`), conflicting checkpoints are refused
  (`audit_log.checkpoint_conflict`), and `prune` drops record bodies but keeps hashes.
- **`verify` and `adk-rust audit`** (`adk-auth`, `adk-cli`): `signed_audit::verify` reports
  flipped bytes, deleted, reordered, or truncated records, forks, untrusted signatures, and
  rotations that are not dual-signed. `adk-rust audit verify` and `audit keygen` (feature `audit`,
  on by default) and `audit export` (feature `audit-sql`) expose it on the command line;
  a build without `audit` drops the subcommands and the `adk-auth` dependency.
