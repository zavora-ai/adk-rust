# Subject Rights

A person's records spread over every store an agent writes: sessions and their `user:` tier,
memory, artifacts, the audit log, and the stores a deployment adds. `adk_core::subject` gives a
deployment the mechanisms to know whom it holds data about, record what they consented to, and
export, erase, or link their data across stores in one operation.

> **Note:** these are mechanisms. Whether a deployment's use of them meets an obligation it has is
> for the deployment to assess.

## Overview

| Type | Role |
|------|------|
| `SubjectKind::Visitor` | A person the deployment knows only by an id it issued, such as a website visitor who has not signed in |
| `VisitorId` | The random id: `v:` and 64 hex characters, never derived from an address, user agent, or fingerprint |
| `ConsentStore` | Consent records per subject and purpose; gates read it before a lasting write |
| `SubjectRegistry` | Each subject's pseudonym, links, and erasure state — never its data |
| `SubjectDataStore` | One store family's adapter: export and erase a subject's data |
| `SubjectRights` | Fans `export`, `erase`, and `link` out over the registry, the consent store, and every adapter |

The registry holds no index of a subject's records. Stores keyed by `(app, user)` find a subject by
its id, so no second copy of a person's data can drift from the stores that hold it.

## Visitors

A visitor is pseudonymous, not anonymous: the id names one person, so the data held under it is that
person's. A visitor's run is never authenticated:

| Check | A visitor |
|-------|-----------|
| `ReadonlyContext::is_authenticated()` | `false`, even when the run carries a `RequestContext` |
| Standing approvals and requested calls | Never apply, since both need an authenticated user |
| `ApprovalService::decide` | Refused with `approval.visitor_not_allowed` |

Set `ActingConfig::subject_kind` (or `RunConfigBuilder::subject_kind`) to `SubjectKind::Visitor` for
a visitor's run.

```rust
use adk_core::subject::VisitorId;
use adk_core::{RunConfig, SubjectKind};

let visitor = VisitorId::generate();
let config = RunConfig::builder().subject_kind(SubjectKind::Visitor).build();
assert_eq!(config.acting.subject_kind, SubjectKind::Visitor);
assert!(visitor.as_str().starts_with("v:"));
```

## Consent

`ConsentPurpose` names what a lasting write is for. The names match AWP's consent scopes one to one,
so a signed AWP receipt is the evidence of the record with the same purpose.

| Purpose | Lasting writes it allows |
|---------|--------------------------|
| `SessionContinuity` | A conversation kept beyond the visit |
| `PersistentProfile` | The `user:` state tier and long-term memory |
| `Analytics` | User attributes on telemetry; visit counting |
| `ProactiveOutreach` | Messages the site starts |
| `PaymentData` | Stored payment preferences |
| `Newsletter` | The site's newsletter |
| `Custom(name)` | A purpose the deployment defines |

A `ConsentRecord` is live from `granted_at` until it expires or is revoked. Revoking stops later
writes and erases nothing, unless `SubjectRights::with_erase_on_revoke` names the purpose.

```rust
use std::sync::Arc;

use adk_core::subject::{
    ConsentEvidence, ConsentPurpose, ConsentRecord, ConsentStore, InMemoryConsentStore,
    InMemorySubjectRegistry, SubjectRights, VisitorId,
};

# async fn demo() -> adk_core::Result<()> {
let rights = SubjectRights::new(
    Arc::new(InMemorySubjectRegistry::new()),
    Arc::new(InMemoryConsentStore::new()),
)
.with_app("shop")
.with_erase_on_revoke(ConsentPurpose::PersistentProfile);

let visitor = VisitorId::generate().subject();
rights
    .grant(
        ConsentRecord::new(visitor.clone(), ConsentPurpose::PersistentProfile, "2026-10")
            .with_evidence(ConsentEvidence::new("awp.receipt", "6f1c…")),
    )
    .await?;

// Revoking this purpose erases the visitor's data as well.
let revocation = rights.revoke(&visitor, Some(&ConsentPurpose::PersistentProfile)).await?;
assert!(revocation.erasure.is_some());
# Ok(())
# }
```

## Erasure

`SubjectRights::erase` works in this order, and can be repeated until it completes:

1. **Mark.** The subject and every visitor id linked to it move to `SubjectState::Erasing`; consent
   gates refuse writes for a subject in that state with `subject.erasing`.
2. **Revoke.** Every live consent record is revoked.
3. **Erase.** Each `SubjectDataStore` erases the subject's data by its `StoreSupport`.
4. **Consent.** The consent records are erased.
5. **Audit.** An `erasure` audit record names the subject's pseudonym and each store's outcome, never
   the subject's id.
6. **Forget.** When no store failed, the registry forgets the subject, its pseudonym, and its links.
   A failed store leaves the subject `Erasing`, so a repeat finishes the job.

| `StoreSupport` | `erase` | Report line |
|----------------|---------|-------------|
| `Hard` | Deletes before returning | `erased` with counts |
| `Eventual { within }` | Starts an asynchronous delete | `scheduled` with `completesBy` |
| `Unsupported` | Not called; the data remains | `unsupported` |
| `Unreachable` | Not called; the data is held outside the deployment, such as by a model provider | `unreachable` |
| any, when the call fails | — | `failed` with the error code |

The report's `status` is `completed` when every store erased or scheduled its deletion, and `partial`
otherwise. A store that cannot erase is reported, never skipped. The report names no subject, so it
may be kept or logged.

```rust
use std::sync::Arc;

use adk_core::subject::{
    ErasureStatus, InMemoryConsentStore, InMemorySubjectRegistry, StoreErasure, StoreSupport,
    SubjectDataStore, SubjectRights, SubjectScope, VisitorId,
};
use adk_core::{Result, async_trait};
use serde_json::{Value, json};

/// The data a provider keeps, which the deployment cannot delete itself.
struct ProviderResponses;

#[async_trait]
impl SubjectDataStore for ProviderResponses {
    fn name(&self) -> &str {
        "provider.responses"
    }

    fn support(&self) -> StoreSupport {
        StoreSupport::Unreachable
    }

    async fn export(&self, _scope: &SubjectScope) -> Result<Value> {
        Ok(json!(null))
    }

    async fn erase(&self, _scope: &SubjectScope) -> Result<StoreErasure> {
        Ok(StoreErasure::default())
    }
}

# async fn demo() -> adk_core::Result<()> {
let rights = SubjectRights::new(
    Arc::new(InMemorySubjectRegistry::new()),
    Arc::new(InMemoryConsentStore::new()),
)
.with_app("shop")
.with_store(Arc::new(ProviderResponses));

let report = rights.erase(&VisitorId::generate().subject()).await?;
assert_eq!(report.status, ErasureStatus::Partial);
assert!(!report.failed(), "nothing to retry: the provider is reported, not retried");
# Ok(())
# }
```

## Export

`SubjectRights::export` returns the registry records, every consent record, and one entry per store
for the subject and its linked visitor ids. A store that cannot list by subject returns
`subject.unsupported` and appears as `unsupported`; `SubjectExport::is_complete` says whether every
store returned its data.

## Linking a visitor to an account

`SubjectRights::link(visitor, identified)` records that a visitor id belongs to an identified
subject. From then on, export and erasure of the identified subject cover the visitor id. Data stays
where it is; moving it under the account is a separate, later step.

| Rule | Enforced by |
|------|-------------|
| A visitor id links to one subject, once | `subject.link.consumed` from the registry |
| Only a visitor links, and only to a subject that is not a visitor | `subject.link.invalid` |
| Neither subject is being erased | `subject.erasing` |
| The caller proved the visitor's token and a fresh authentication of the identified subject in the same request | The server, before it calls `link` |

The link is audited under both subjects' pseudonyms.

## Backends

The registry and the consent store run on every session backend (`adk-session`):

| Backend | Feature | Subject registry | Consent store |
|---------|---------|------------------|---------------|
| In-memory (`adk-core`) | — | `InMemorySubjectRegistry` | `InMemoryConsentStore` |
| SQLite | `sqlite` | `SqliteSubjectRegistry` | `SqliteConsentStore` |
| PostgreSQL | `postgres` | `PostgresSubjectRegistry` | `PostgresConsentStore` |
| Redis | `redis` | `RedisSubjectRegistry` | `RedisConsentStore` |
| MongoDB | `mongodb` | `MongoSubjectRegistry` | `MongoConsentStore` |
| Neo4j | `neo4j` | `Neo4jSubjectRegistry` | `Neo4jConsentStore` |
| Firestore | `firestore` | `FirestoreSubjectRegistry` | `FirestoreConsentStore` |

| Store family | Adapter | Crate | Erases |
|--------------|---------|-------|--------|
| Sessions | `SessionSubjectData` | `adk-session` | Every session, its events, and the `user:` tier through `delete_all_sessions`, on any `SessionService` |

`SessionSubjectData::with_support(StoreSupport::Eventual { within })` declares a session
service whose deletes complete asynchronously, such as Vertex AI's.

## Writing an adapter

A store family implements `SubjectDataStore` once and proves it with the shared suite:

| Case | Contract |
|------|----------|
| `export_returns_only_the_subject` | A seeded subject's export holds data; that of a subject never seeded is empty |
| `erase_removes_only_the_subject` | No read path returns the subject's data afterwards; another subject's data is intact |
| `erase_is_idempotent` | A repeat succeeds and deletes nothing |
| `erase_covers_linked_ids` | The data of linked visitor ids goes too |
| `erase_stays_in_its_applications` | Data in an application outside the scope stays |

```rust,ignore
use std::sync::Arc;

use adk_core::async_trait;
use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::subject::SubjectDataStore;

struct Harness;

#[async_trait]
impl SubjectDataStoreHarness for Harness {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        todo!("the adapter under test")
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        todo!("write data for the user through the store's own API")
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        todo!("count what every read path returns for the user")
    }
}

adk_core::subject_data_store_conformance_tests!(harness = Harness);
```

`subject_registry_conformance_tests!` and `consent_store_conformance_tests!` prove registry and
consent store backends the same way. All three need the `conformance` feature of `adk-core`.

## Error codes

| Code | Category | Meaning |
|------|----------|---------|
| `subject.erasing` | `Conflict` | The subject is being erased; nothing is written for it |
| `subject.link.consumed` | `Conflict` | The visitor id is already linked |
| `subject.link.invalid` | `InvalidInput` | A link names subjects of the wrong kinds |
| `subject.unsupported` | `Unsupported` | A store cannot export or erase by subject |
| `consent.conflict` | `Conflict` | A consent record with the id exists |
| `approval.visitor_not_allowed` | `Forbidden` | A visitor tried to decide an approval request |

## Related

- [Agent Identity](agent-identity.md) — subject kinds and authenticated identity
- [Audit Log](audit-log.md) — the signed log erasure records are written to
- [AWP Edge](../deployment/awp-edge.md) — consent receipts at the website edge
