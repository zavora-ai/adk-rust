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

## What the runtime records

Give the runner an audit log and every run records its governed actions — no tool, callback,
or plugin needs to write a record itself:

```rust
use std::sync::Arc;

use adk_auth::signed_audit::AuditLog;
use adk_core::audit_log::AuditFailureMode;
use adk_core::{Agent, DeclarativePolicy, Result};
use adk_runner::Runner;
use adk_session::SessionService;

fn audited_runner(
    agent: Arc<dyn Agent>,
    sessions: Arc<dyn SessionService>,
    log: Arc<AuditLog>,
) -> Result<Runner> {
    Runner::builder()
        .app_name("bank")
        .agent(agent)
        .session_service(sessions)
        .tool_policy(Arc::new(
            DeclarativePolicy::builder()
                .name("bank-v1")
                .allow("search")
                .require_approval("transfer", "transfers need a person")
                .build(),
        ))
        .audit_log(log)
        .audit_failure_mode(AuditFailureMode::Block)
        .build()
}
```

`audit_log` takes any `AuditRecorder`; `AuditLog` is one, and `InMemoryAuditRecorder` keeps
drafts in memory for tests. The runner installs the trail on each run's `RunConfig::audit`
(set it there to audit a single run) and attaches it to the runner's kill switch.

| Event | Where | `kind` | `action` | `decision` / `outcome` | `policyRef` |
|-------|-------|--------|----------|------------------------|-------------|
| Tool call allowed | Governed path, before `execute` | `tool_call` | Tool name | `allowed` | `tool_policy[:<name>]`, `confirmation`, or absent with no policy |
| Tool call denied | Governed path | `tool_call` | Tool name | `denied` / `not_run` | `tool_policy[:<name>]`, `guardrail`, `governance` (frozen), `confirmation` |
| Tool call held for approval | Governed path | `tool_call` | Tool name | `approval_required` / `not_run` | `tool_policy[:<name>]` or `confirmation` |
| Approval decision applied | Governed path, before the call's decision | `approval` | Tool name | `approved` or `rejected` | — |
| Approver decision | `StoreApprovalService::with_audit`, before the decision is stored | `approval` | Tool name | `approved` (approve, edit) or `rejected` (reject, respond) | — |
| Freeze, unfreeze | `GovernanceControl` | `governance` | `governance.freeze`, `governance.unfreeze` | — / `succeeded` | — |
| Payment decision | Payment tools (ACP checkout, AP2 mandate) | `payment` | `payment.create_checkout`, `payment.complete_checkout`, `payment.execute_payment` | `allowed`, `approved`, `denied`, `approval_required`, `rejected` | `payment_policy` |
| Payment outcome | Payment tools, after the backend returns | `payment` | As above | `succeeded` or `failed` | `payment_policy` |
| Outcome unknown | Agents (timeout, panic, replay of a begun call), runner (cancelled run) | `tool_call` | Tool name | `outcome_unknown` | — |

Every record names the acting agent and the subject it acts for in `actor`: the agent id
`urn:adk:agent:<app>/<agent>`, `onBehalfOf` the session's user, and the invocation id.
Records never carry arguments: `argsDigest` is `sha256:<hex>` of the call's final arguments
(the cart for a payment). `ext` holds the call ID, the tool's declared effect, the session
ID, the policy's reason for a denial or hold, an approval's `source` (`call_id`,
`fingerprint`, `approval_store`, `handler`, `handler_timeout`), an unknown outcome's `cause`
(`interrupted`, `began_without_outcome`, `cancelled`), and a payment's transaction ID,
merchant, total, and the guardrails that spoke. A guardrail screen's refusal text can quote
the arguments, so it stays out of the record.

`DeclarativePolicyBuilder::name` names a policy, and a custom `ToolPolicy` names itself by
overriding `policy_ref`; records then cite it as `tool_policy:<name>`.

### Failure mode

| `AuditFailureMode` | A record cannot be written |
|--------------------|----------------------------|
| `Block` (default) | The action does not happen: the governed call is not executed and the run ends with `audit.record_failed`; an allowed payment is refused before the backend is reached; `unfreeze_recorded` leaves execution frozen |
| `Warn` | The failure is logged and the action proceeds without a record |

A record that follows an action that already happened cannot refuse it: a freeze holds even
when its record fails, and a payment's outcome record, a refusal's record, and a cancelled
run's outcome-unknown records are logged on failure.

An approver's decision through the approval service names the approver as the actor, with
the delegator they acted for in `delegationChain`, and carries `approvalId`,
`approvalAction`, and the digest of any edited arguments; under `Block` a decision that
cannot be recorded is refused. When the run later applies the stored decision, the governed
path writes its own `approval` record with `source` `approval_store`.

`GovernanceControl::freeze_recorded` and `unfreeze_recorded` take the acting administrator
and wait for the record; on a mirrored control they write the shared governance store, as
`freeze_replicated` and `unfreeze_replicated` do, with the administrator as the issuer; the `POST /api/admin/freeze` and `unfreeze` endpoints use them with
the authenticated user, and an unrecordable unfreeze answers `503`. `freeze` and `unfreeze`
record in the background.

Spend-ledger commits are not recorded: the spend ledger is itself the durable record of
spend, a model call commits on every turn, and a refused reservation ends the run with
`ResourceExhausted` before any action happens.

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
| Redis | `adk_session::RedisAuditLogStore` | `redis` |
| MongoDB | `adk_session::MongoAuditLogStore` | `mongodb` |
| Neo4j | `adk_session::Neo4jAuditLogStore` | `neo4j` |
| Firestore | `adk_session::FirestoreAuditLogStore` | `firestore` |

| Backend | How two writers on one `log_id` are serialized |
|---------|------------------------------------------------|
| SQLite | `BEGIN IMMEDIATE` takes the write lock before the size is read |
| PostgreSQL | `SELECT ... FOR UPDATE` on the log's size row |
| Redis | One Lua script compares the first `seq` with the size and writes entries, nodes, and size together |
| MongoDB | A conditional update on the log document claims the size and parks the batch; a writer that finds a parked batch completes it first |
| Neo4j | A transaction write-locks the log node before it reads the size |
| Firestore | A read-write transaction reads the log document and creates entries with an `Exists(false)` precondition |

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
