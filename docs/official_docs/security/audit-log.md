# Audit Log

`adk-auth` (feature `signed-audit`) writes a signed, verifiable audit log. Each replica
appends records to its own log; the log signs checkpoints of a Merkle tree over the records,
and anyone holding the public key can verify an export offline, with `adk_auth::signed_audit::verify`
or `adk-rust audit verify`.

| Layer | Mechanism | Detects |
|-------|-----------|---------|
| Record | `AuditRecord`, canonicalized with RFC 8785 (JCS) | Any change to a field |
| Chain | `prev_hash` is the previous record's leaf hash | A deleted, inserted, or reordered record |
| Tree | RFC 9162 Merkle tree, leaf `SHA-256(0x00 ‖ record)`, with inclusion and consistency proofs | A record outside the signed tree; a rewritten prefix |
| Checkpoint | C2SP tlog checkpoint in a signed note, Ed25519 | A rewrite by anyone without the signing key |
| Anchor | Optional publication of checkpoints outside the log | A rewrite by the key holder |

Checkpoint notes use the format and key encoding of Go's `golang.org/x/mod/sumdb/note`, so
standard signed-note verifiers and witnesses accept them.

## Setup

```toml
[dependencies]
adk-auth = { version = "3.0.0", features = ["signed-audit"] }
adk-core = "3.0.0"
adk-session = { version = "3.0.0", features = ["postgres"] }
```

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_auth::signed_audit::{AuditLog, Ed25519Signer, FileCheckpointAnchor};
use adk_core::audit_log::{AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind};
use adk_session::PostgresAuditLogStore;

async fn open_audit_log(instance: &str) -> adk_core::Result<Arc<AuditLog>> {
    let store = PostgresAuditLogStore::new("postgres://localhost/app").await?;
    store.migrate().await?;

    let key = std::fs::read_to_string("/etc/adk/audit.key").expect("signer key");
    let signer = Arc::new(Ed25519Signer::from_note_key(key.trim()).expect("valid key"));

    let log = AuditLog::builder(Arc::new(store), format!("audit/{instance}"), signer)
        .origin(format!("audit.example.com/{instance}"))
        .checkpoint_every(256)
        .checkpoint_interval(Duration::from_secs(60))
        .anchor(Arc::new(FileCheckpointAnchor::new("/mnt/worm/audit-checkpoints.jsonl")))
        .open()
        .await?;
    let log = Arc::new(log);
    log.spawn_checkpoint_timer();

    log.append(
        AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer")
            .actor(AuditActor::default())
            .decision(AuditDecision::Allowed)
            .args_digest(adk_auth::signed_audit::json_sha256(&serde_json::json!({ "amount": 50 }))?),
    )
    .await?;
    Ok(log)
}
```

`AuditLog` implements `AuditSink`, so `ProtectedTool`, `ScopedTool`, and `AuthMiddleware`
write their access decisions into the signed log with no other change:

```rust
use std::sync::Arc;

use adk_auth::{AccessControl, AuthMiddleware};
use adk_auth::signed_audit::AuditLog;

fn middleware(access: AccessControl, log: Arc<AuditLog>) -> AuthMiddleware {
    AuthMiddleware::with_audit(access, log)
}
```

The adapter maps the event type to the record kind, the event's user to
`actor.onBehalfOf`, and the outcome to `decision` or `outcome`; the remaining event fields
become `ext` entries.

## Records

| Field | Set by | Meaning |
|-------|--------|---------|
| `v` | writer | Schema version, `1` |
| `logId` | writer | The replica's log |
| `seq` | writer | Position, from 0 with no gaps |
| `ts` | writer | Append time, from the configured `Clock` |
| `prevHash` | writer | Leaf hash of the previous record; absent for `seq` 0 |
| `actor` | caller | Agent id, tenant, instance, `onBehalfOf`, delegation chain, invocation id — `AuditActor::from(&ActingContext)` |
| `kind` | caller | `tool_call`, `approval`, `governance`, `payment`, `access`, `authentication`, `configuration`, `key_rotated`, or any other name |
| `action` | caller | Tool name or operation |
| `argsDigest` | caller | `sha256:<hex>` of the canonical arguments (`json_sha256`) |
| `decision` | caller | `allowed`, `denied`, `approval_required`, `approved`, `rejected` |
| `outcome` | caller | `succeeded`, `failed`, `outcome_unknown`, `not_run` |
| `policyRef` | caller | The deciding policy rule or version |
| `payloadRef` | caller | Digest of a payload stored outside the log |
| `ext` | caller | Extension fields; integers must stay within ±2^53 (I-JSON) |

### Personal and large data

Keep personal data and large payloads out of records. Store the payload in a separate store,
ideally encrypted under a per-subject key, and record only its digest in `payloadRef`:

1. Compute `json_sha256(&payload)` and store the payload under that digest.
2. Append the record with `payload_ref(digest)`.
3. To erase a subject, delete the payload or destroy its key (crypto-shredding). The record,
   the chain, and every checkpoint stay verifiable because they hold only the digest.

`AuditLogStore::prune(log_id, before_seq)` drops record bodies older than a position and
keeps their leaf hashes, so a pruned log still verifies; the report counts pruned entries.

## Checkpoints and keys

A checkpoint is signed every `checkpoint_every` records, on the first append after
`checkpoint_interval`, on `checkpoint()`, and on `AuditSink::flush`. `spawn_checkpoint_timer`
signs idle logs on the interval.

```text
audit.example.com/replica-1
1024
zwu4qZLzhb2lbYJkhqUVd7RS2SAPmggiFWFGeL5A3gc=

— audit.example.com EO0LDO4YVlTf02uEtzCDXE+PCs0w3LJgo57v2QVSBz+Lns0lo...
```

| Task | How |
|------|-----|
| Generate a key | `adk-rust audit keygen audit.example.com --out audit` writes `audit.key` (mode 0600) and `audit.vkey` |
| Load a key | `Ed25519Signer::from_note_key(...)`; implement `AuditSigner` to sign with a KMS or HSM |
| Rotate a key | `log.rotate_signer(next, actor)` appends a `key_rotated` record naming both verifier keys and signs the next checkpoint with both |
| Trust after rotation | A verifier that trusts the old key learns the new one from the dual-signed checkpoint and retires the old one for later checkpoints |

`CheckpointAnchor` publishes each checkpoint outside the operator's control. The crate ships
`FileCheckpointAnchor`, which appends checkpoints to a file on a write-once mount or another
host; implement the trait for a witness, an RFC 3161 timestamp authority, or a transparency
log. An anchor failure is logged and does not fail the append.

## Stores

`AuditLogStore` persists what the writer computes: entries (canonical record and leaf hash),
complete-subtree hashes, and signed checkpoints. It never computes hashes itself.

| Backend | Type | Feature |
|---------|------|---------|
| In memory | `adk_core::audit_log::InMemoryAuditLogStore` | `adk-core` |
| SQLite | `adk_session::SqliteAuditLogStore` | `sqlite` |
| PostgreSQL | `adk_session::PostgresAuditLogStore` | `postgres` |

Give every replica its own `log_id`; no shared sequencer is needed. `append` is a
compare-and-swap on the log size, so a second writer on one `log_id` gets
`audit_log.seq_conflict` instead of forking the log. A second checkpoint at one size with a
different root is refused with `audit_log.checkpoint_conflict`. Every backend passes
`audit_log_store_conformance_tests!` (see [Durable Stores](durable-stores.md)).

## Verifying

Export a log and verify it with the keys you trust:

```bash
adk-rust audit export --database-url postgres://localhost/app --log-id audit/replica-1 --out audit.jsonl
adk-rust audit verify audit.jsonl --key "$(cat audit.vkey)" --checkpoints /mnt/worm/audit-checkpoints.jsonl
```

`audit verify` and `audit keygen` need the `audit` feature of `adk-cli`, which is on by
default; `audit export` needs `audit-sql`. `audit verify` exits 0 when the log
verifies and 1 when it does not; `--json` prints the report as JSON.

```rust
use adk_auth::signed_audit::{AuditExport, NoteVerifier};

fn check(export_jsonl: &str, verifier_key: &str) -> bool {
    let export = AuditExport::from_jsonl(export_jsonl).expect("valid export");
    let trusted: NoteVerifier = verifier_key.parse().expect("valid verifier key");
    let report = export.verify(&[trusted]);
    println!("{report}");
    report.is_valid()
}
```

| Failure | Raised when |
|---------|-------------|
| `leaf_hash_mismatch` | A record's text does not hash to its leaf hash (a flipped byte) |
| `chain_broken` | `prevHash` does not match the previous leaf (an edited record with a recomputed hash, a deletion, a reorder) |
| `sequence_broken` | Entries are missing, duplicated, or out of order |
| `root_mismatch` | A checkpoint's root differs from the tree over the records |
| `truncated` | A checkpoint covers more records than the export holds |
| `fork` | Two signed checkpoints of one size disagree, such as a rewritten log against its anchored checkpoints |
| `untrusted_checkpoint` | No valid signature from a trusted, unretired key |
| `rotation_not_dual_signed` | The checkpoint after a `key_rotated` record lacks either key's signature |
| `not_canonical`, `malformed`, `log_id_mismatch`, `origin_mismatch`, `unsupported_version` | The export is not a well-formed log |
| `unanchored` | Records exist but no checkpoint signs them |

> **Note:** Records after the last checkpoint are reported as `unsignedTail`. Removing them is
> not detectable until a later checkpoint or an anchor covers them; a short
> `checkpoint_interval` and an anchor bound that window. A key holder can rewrite and re-sign
> the whole log; anchored checkpoints expose that as a `fork`.

`AuditLog::inclusion_proof` and `AuditLog::consistency_proof` return RFC 9162 proofs, and
`signed_audit::merkle::verify_inclusion` and `verify_consistency` check them, so a client can
confirm one record, or that a newer checkpoint extends an older one, without the full log.

## Related

- [Access Control](access-control.md)
- [Durable Stores](durable-stores.md)
- [Tool Authorization](tool-authorization.md)
