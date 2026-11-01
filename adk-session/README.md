# adk-session

Session management and state persistence for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-session.svg)](https://crates.io/crates/adk-session)
[![Documentation](https://docs.rs/adk-session/badge.svg)](https://docs.rs/adk-session)
[![License](https://img.shields.io/crates/l/adk-session.svg)](../LICENSE)

## Overview

`adk-session` provides session and state management for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemorySessionService** - Simple in-memory session storage
- **SqliteSessionService** - SQLite-backed persistence (`sqlite` feature)
- **Durable stores** - `SpendLedger`, `ActionLedger`, and `ApprovalStore` for SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore, each behind its backend's feature and proven by the `adk-core` conformance suites; the approval stores also hold the requests and delegations of the `adk-core` approval service
- **Durable runtime stores** - `LeaseStore`, `DurableRunStore`, `ScheduleStore`, and `GovernanceStateStore` for SQLite, PostgreSQL, Redis, MongoDB, Neo4j, and Firestore, each proven by the `adk-core` conformance suites
- **PostgresSessionService** - PostgreSQL-backed persistence (`postgres` feature)
- **RedisSessionService** - Redis-backed persistence on a standalone server or Redis Cluster, where each session's keys share one hash tag (`redis` feature)
- **MongoSessionService** - MongoDB-backed persistence (`mongodb` feature)
- **Neo4jSessionService** - Neo4j-backed persistence (`neo4j` feature)
- **FirestoreSessionService** - Firestore-backed persistence (`firestore` feature)
- **VertexAiSessionService** - Vertex AI Session API backend (`vertex-session` feature)
- **Schema Migrations** - Versioned, forward-only migrations for all database backends
- **Identifier validation** - Every backend rejects an empty, oversized, or NUL-containing app name, user ID, or session ID with an `InvalidInput` error before it touches storage
- **One session contract** - On every backend, `create` for an existing identity fails with `session.already_exists` and writes nothing, appends to a missing session fail with `session.not_found`, events with equal timestamps keep their append order, and `list` returns the most recently updated session first; the shared conformance suite checks each backend
- **Rewind** - `rewind` and `rewind_steps` address a session by its full identity and keep the state it was created with (in-memory, SQLite, and `EncryptedSession`)

## Installation

```toml
[dependencies]
adk-session = "3.0.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["sessions"] }
```

## Quick Start

```rust
use std::collections::HashMap;

use adk_session::{CreateRequest, InMemorySessionService, KEY_PREFIX_USER, SessionService};
use serde_json::json;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemorySessionService::new();

    let mut initial_state = HashMap::new();
    initial_state.insert(format!("{KEY_PREFIX_USER}name"), json!("Alice"));

    let session = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: None,
            state: initial_state,
        })
        .await?;

    assert_eq!(session.state().get("user:name"), Some(json!("Alice")));
    Ok(())
}
```

`create` for an identity that already exists fails with `session.already_exists` (`Conflict`) and writes nothing.

## State Prefixes

| Prefix | Purpose | Persistence |
|--------|---------|-------------|
| `user:` | User preferences | Shared by every session of the user |
| `app:` | Application state | Shared by every session of the app |
| `temp:` | Temporary data | Not persisted; the runner keeps it for the rest of the invocation |

`get` and `list` read the `app:` and `user:` tiers at call time, so a value written through one session reaches the app's or user's other sessions.

`delete_all_sessions(app, user)` erases the user's sessions in the app, their events, and the user's `user:` tier, on every backend; the `app:` tier and other users' data stay. Vertex AI keeps `user:` keys in each session and erases them with the sessions.

## Feature Flags

| Feature | Backend | Description |
|---------|---------|-------------|
| `sqlite` | SQLite | Single-node persistence via sqlx |
| `database` | SQLite | Alias for `sqlite` |
| `postgres` | PostgreSQL | Production-grade relational persistence |
| `redis` | Redis | Low-latency in-memory persistence via fred |
| `mongodb` | MongoDB | Document-oriented persistence |
| `neo4j` | Neo4j | Graph database persistence |
| `firestore` | Firestore | Google Cloud Firestore persistence |
| `vertex-session` | Vertex AI | Vertex AI Session API backend |
| `encrypted-session` | AES-256-GCM | Transparent encryption at rest with key rotation |

```toml
# SQLite
adk-session = { version = "3.0.0", features = ["sqlite"] }

# PostgreSQL
adk-session = { version = "3.0.0", features = ["postgres"] }

# Redis
adk-session = { version = "3.0.0", features = ["redis"] }

# Encrypted sessions
adk-session = { version = "3.0.0", features = ["encrypted-session"] }

# Vertex AI sessions through the umbrella crate
adk-rust = { version = "3.0.0", features = ["vertex-session"] }
```

## Vertex AI Sessions

`VertexAiSessionService` stores sessions through the GA `v1` Vertex AI Agent
Engine Session API:

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

fn main() -> adk_core::Result<()> {
    let config = VertexAiSessionConfig::new("my-project", "us-central1")
        .with_reasoning_engine("1234567890");
    let _service = VertexAiSessionService::new_with_adc(config)?;
    Ok(())
}
```

Inside a deployed Vertex AI Agent Engine container,
`VertexAiSessionConfig::from_env()` reads `GOOGLE_CLOUD_PROJECT`,
`GOOGLE_CLOUD_LOCATION`, and `GOOGLE_CLOUD_AGENT_ENGINE_ID` (the bare numeric
engine ID) directly from the platform-provided environment.
`with_ttl()`/`with_expire_time()` set the `Session.expiration` oneof sent on
session create; `ttl` has a 24-hour minimum and both members are mutually
exclusive.

Caller-facing session IDs remain unchanged. The backend derives a deterministic
remote ID from the complete `(app_name, user_id, session_id)` identity and stores
a protected identity marker in Vertex session state. The marker is removed from
all returned state and cannot be supplied through create state or event state
deltas. This permits the same logical session ID to exist safely across apps and
users, including when several apps share one reasoning engine.

The default endpoint depends on the configured location:

| Location | Endpoint |
|----------|----------|
| `global` | `https://aiplatform.googleapis.com` |
| `us` | `https://aiplatform.us.rep.googleapis.com` |
| `eu` | `https://aiplatform.eu.rep.googleapis.com` |
| Region such as `us-central1` | `https://us-central1-aiplatform.googleapis.com` |

### Production boundaries

| Boundary | Behavior |
|----------|----------|
| Vertex user ID | At most 128 Unicode scalar values |
| Encoded request body | 64 MiB per create/append request |
| Decoded response body | 64 MiB per response and in aggregate for one paginated list operation |
| Pagination | 120-second total deadline; recent-event bounds and timestamp filters are pushed into the API |
| Page token | At most 64 KiB |
| Transport | 10-second connect, 30-second credential-header, and 120-second HTTP request deadlines |
| Long-running operations | 120-second local polling deadline |
| JSON and Struct values | At most 64 nested levels |

Use `with_max_request_bytes()`, `with_max_response_bytes()`, and
`with_pagination_timeout()` to select deployment-specific bounds. Raising a
body limit weakens the default allocation protection; neither body-byte budget
is a total process-memory ceiling.

Custom endpoints are security-sensitive: the origin receives Google
authorization headers and complete session/event payloads. The caller must
configure a trusted endpoint. The constructor accepts an HTTPS origin shape
without userinfo, a path, query, or fragment; loopback HTTP is available for
tests. Redirects are disabled.

Create, delete, and append HTTP `408`/`5xx`, transport/body, malformed `2xx`,
or invalid successful-result failures can occur after the service commits the
mutation. They return
`session.vertex.{create,delete,append}_outcome_ambiguous` with
`retry.should_retry = false`. After create or delete returns an operation name,
poll transport/status failures and malformed or invalid successful responses
use the same non-retryable ambiguity contract and include the operation name.
Inspect the target session, operation, or event list before any manual retry.
A terminal `done: true` operation error is a known failed result instead: it
retains `session.vertex.operation_failed` and its category-derived retry hint.

Direct HTTP `4xx` rejections other than `408` remain definitive; `429` is
retryable. Any poll failure after an operation is accepted, including `429`, is
ambiguous and non-retryable. The backend does not throttle or retry internally.
Size capacity using the current [Vertex AI
quotas](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/quotas).

Run the ignored GA `v1` canary only against an existing reasoning engine with
Application Default Credentials configured. It creates and deletes one session
and one representative event; it never creates or deletes the engine.

```bash
ADK_VERTEX_LIVE_TEST=1 \
GOOGLE_CLOUD_PROJECT=PROJECT_ID \
GOOGLE_CLOUD_LOCATION=LOCATION \
GOOGLE_CLOUD_REASONING_ENGINE_ID=REASONING_ENGINE_ID \
cargo test -p adk-session --features vertex-session \
  --test session_contract_vertex test_vertex_live_ga_v1_canary \
  -- --ignored --exact --nocapture
```

Arbitrary Google `rawEvent` Struct values remain opaque. Reappend preserves
every original key and value and adds the reserved `_adkRust` envelope; removing
that envelope yields the original Struct. A pre-existing malformed `_adkRust`
key fails closed. Google ADK projection is best-effort even when a Struct has a
non-empty string `id`, numeric `timestamp`, and string `invocationId` and
`author`; incompatible content, actions, metadata, or optional fields do not
reject the canonical SessionEvent.

The GA `v1` `FunctionCall` and `FunctionResponse` wire messages have no `id`.
ID-bearing ADK function parts and empty or noncanonical Base64 thought-signature
bytes use the lossless private `rawEvent` path. `Part.mediaResolution` accepts a
bounded JSON object on any otherwise-valid canonical part. Top-level
`inlineData`/`fileData` `displayName` values are accepted from the deployed GA
wire. ADK projection omits these provider-only fields, while the canonical
sidecar preserves them across reappend. Private-envelope validation treats
omitted proto3 default scalar fields as equivalent to their empty private
values, accepts safe integer/float normalization inside Vertex Struct values,
and restores exact private scalar presence.

### Legacy unmarked sessions

When `reasoning_engine` is omitted, `app_name` must be the canonical nonzero
numeric reasoning-engine ID without leading zeros. That ID selects the isolated
parent, and the backend can access unmarked Python ADK or pre-v2 sessions by
their direct remote ID. Logical app names and full resource names require
`with_reasoning_engine()`. When a fixed reasoning engine is shared, unmarked
access is disabled by default. Enable it for exactly one audited app during
migration:

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

fn main() -> adk_core::Result<()> {
    let legacy_config = VertexAiSessionConfig::new("my-project", "us-central1")
        .with_reasoning_engine("1234567890");
    let _service = VertexAiSessionService::new_with_adc(legacy_config)?
        .allow_unmarked_sessions_for_app("legacy-app");
    Ok(())
}
```

> **Important:** Unmarked sessions carry no app marker. Only enable this
> compatibility mode when the selected app exclusively owns the legacy
> sessions in that reasoning engine. The backend still requires the exact
> `user_id` and rejects marked-direct, reserved-ID, and computed/direct
> ambiguities.

Use `append_event_for_identity()` for new code. The legacy
`append_event(session_id, event)` method works only after a create, get, or list
operation has cached exactly one app/user scope for that ID. The cache is
bounded; long-running processes may need to get or list an older session again
after its scope is evicted.

## Firestore Sessions

`FirestoreSessionService` stores a session at `{root}/{app_name}/sessions/{session_id}`
with the owning `user_id` as a document field. Every write re-reads the session
document inside its transaction and applies only when the app, user, and session ID
match the request:

| Operation | Session missing, or owned by another user |
|-----------|-------------------------------------------|
| `create` | Fails with `session.already_exists` (`Conflict`) when the ID is taken in the app |
| `get`, `append_event_for_identity` | Fails with `session.not_found` |
| `delete` | No-op, identical to deleting a missing session |
| `list`, `delete_all_sessions` | Only the caller's own sessions are returned or removed; `delete_all_sessions` then deletes the user's state document |

App state lives at `{root}/{app_name}/app_state/current` and user state at
`{root}/{app_name}/users/{user_id}/state/current`. App names, user IDs, and session
IDs are Firestore document IDs, so they must not contain `/`.

## Encrypted Sessions

Wrap any `SessionService` with `EncryptedSession` to encrypt session state and event
payloads at rest using AES-256-GCM:

```rust
use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};

fn main() {
    let key = EncryptionKey::generate();
    let inner = InMemorySessionService::new();
    // Use like any SessionService — encryption is transparent.
    let _service = EncryptedSession::new(inner, key, vec![]);
}
```

| Data | Stored in the inner service as |
|------|--------------------------------|
| State values (create state and event `state_delta`) | Encrypted per value; key names stay in plaintext |
| Event content, LLM response metadata, actions, long-running tool IDs | One encrypted envelope in the event content |
| Event ID, timestamp, invocation ID, branch, author | Plaintext |
| `llm_request`, event `provider_metadata` | Not persisted |

- **Identity binding**: each ciphertext is bound to its app, user, session, and state
  key or event ID, so data copied to another session fails to decrypt.
- **Shared tiers**: `app:` and `user:` values are stored encrypted in the shared app
  and user tiers and stay visible to every session of that app or user.
- **Addressing**: `append_event(session_id, ..)` returns
  `session.encryption.identity_required`; use `append_event_for_identity()`.

Key rotation is supported by passing previous keys:

```rust
use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};

fn rotated() -> adk_core::Result<EncryptedSession<InMemorySessionService>> {
    let new_key = EncryptionKey::generate();
    let old_key = EncryptionKey::from_env("OLD_KEY")?;
    Ok(EncryptedSession::new(InMemorySessionService::new(), new_key, vec![old_key]))
}
```

`get` re-encrypts state values still under a previous key and returns any failure.
Stored events cannot be rewritten, so keep a previous key until the sessions whose
events it encrypted are deleted or expire.

Data written by releases before the per-value format (a single `__encrypted_state`
blob plus plaintext event deltas) is rejected with
`session.encryption.unencrypted_data`. Enable `.with_legacy_migration(true)` while
migrating; `get` then re-encrypts that state into the current format.

## Schema Migrations

All database backends (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned migration system. Migrations are forward-only, idempotent, and tracked in a `_schema_migrations` registry table.

```rust
use adk_session::SqliteSessionService;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = SqliteSessionService::new("sqlite:sessions.db?mode=rwc").await?;

    // Run all pending migrations
    service.migrate().await?;

    // Check the current schema version
    let version = service.schema_version().await?;
    println!("Schema version: {version}");
    Ok(())
}
```

Each backend detects pre-existing tables (baseline detection) and registers them as already applied, so `migrate()` is safe to call on both fresh and existing databases. `schema_version()` returns 0 only when the registry table does not exist; a failed read is an error. PostgreSQL `migrate()` holds the advisory lock `PostgresSessionService::ADVISORY_LOCK_KEY` on one connection and closes that connection afterwards. The `migration::sqlite_runner` and `migration::pg_runner` runners take `&'static` migration steps and ASCII registry table names of at most 63 bytes.

## Durable Stores

| Backend | Feature | Spend ledger | Action ledger | Approval store |
|---------|---------|--------------|---------------|----------------|
| SQLite | `sqlite` | `SqliteSpendLedger` | `SqliteActionLedger` | `SqliteApprovalStore` |
| PostgreSQL | `postgres` | `PostgresSpendLedger` | `PostgresActionLedger` | `PostgresApprovalStore` |
| Redis | `redis` | `RedisSpendLedger` | `RedisActionLedger` | `RedisApprovalStore` |
| MongoDB | `mongodb` | `MongoSpendLedger` | `MongoActionLedger` | `MongoApprovalStore` |
| Neo4j | `neo4j` | `Neo4jSpendLedger` | `Neo4jActionLedger` | `Neo4jApprovalStore` |
| Firestore | `firestore` | `FirestoreSpendLedger` | `FirestoreActionLedger` | `FirestoreApprovalStore` |

`SqliteAuditLogStore` (`sqlite`), `PostgresAuditLogStore` (`postgres`), `RedisAuditLogStore`
(`redis`), `MongoAuditLogStore` (`mongodb`), `Neo4jAuditLogStore` (`neo4j`), and
`FirestoreAuditLogStore` (`firestore`) hold the signed audit log written by
`adk_auth::signed_audit::AuditLog`.

Each type has `new(...)`, a constructor over the handle a session service holds
(`from_pool`, `from_client`, `from_database`, `from_graph`, `from_db`), and `migrate()`.
Spend ledgers add `with_limits`, `with_reservation_ttl`, and `with_clock`. Concurrent
reservations never overshoot a cap, `begin` admits one caller per key, and a decision is
taken once. Every type passes the `adk-core` conformance suites
(`tests/store_conformance_*.rs`); see the
[Durable Stores guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/security/durable-stores.md).

```rust
use adk_core::{SpendKey, SpendLimits, SpendPeriod};
use adk_session::SqliteSpendLedger;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let ledger = SqliteSpendLedger::new("sqlite://spend.db?mode=rwc").await?.with_limits(
        SpendLimits::new().limit(SpendKey::org("my-app").per(SpendPeriod::Day), 50_000_000),
    );
    ledger.migrate().await?;
    Ok(())
}
```

## Durable Runtime Stores

| Backend | Feature | Lease store | Run store | Schedule store | Governance state store |
|---------|---------|-------------|-----------|----------------|------------------------|
| SQLite | `sqlite` | `SqliteLeaseStore` | `SqliteRunStore` | `SqliteScheduleStore` | `SqliteGovernanceStateStore` |
| PostgreSQL | `postgres` | `PostgresLeaseStore` | `PostgresRunStore` | `PostgresScheduleStore` | `PostgresGovernanceStateStore` |
| Redis | `redis` | `RedisLeaseStore` | `RedisRunStore` | `RedisScheduleStore` | `RedisGovernanceStateStore` |
| MongoDB | `mongodb` | `MongoLeaseStore` | `MongoRunStore` | `MongoScheduleStore` | `MongoGovernanceStateStore` |
| Neo4j | `neo4j` | `Neo4jLeaseStore` | `Neo4jRunStore` | `Neo4jScheduleStore` | `Neo4jGovernanceStateStore` |
| Firestore | `firestore` | `FirestoreLeaseStore` | `FirestoreRunStore` | `FirestoreScheduleStore` | `FirestoreGovernanceStateStore` |

The SQL types have `new(url)` and `from_pool` over the pool a session service uses; the
Redis, MongoDB, Neo4j, and Firestore types have `new(...)` and a constructor over a shared
handle (`from_client`, `from_database`, `from_graph`, `from_db`). Every type has `migrate()`; the lease, run, and governance stores add `with_clock`. PostgreSQL claims runs
with `SELECT ... FOR UPDATE SKIP LOCKED`, SQLite under `BEGIN IMMEDIATE`, and every worker
write is fenced on the run's lease epoch. Every type passes the `adk-core` conformance
suites; see the
[Durable Runtime guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/core/durable-runtime.md).

```rust
use std::time::Duration;

use adk_core::{DurableRunStore, NewRun, RunOutcome};
use adk_session::PostgresRunStore;
use serde_json::json;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let runs = PostgresRunStore::new("postgres://localhost/app").await?;
    runs.migrate().await?;
    runs.enqueue(NewRun::new("default", "my-app", "user-1", "assistant", json!({"text": "hi"})))
        .await?;
    if let Some(claimed) = runs.claim("default", "worker-1", Duration::from_secs(30)).await? {
        runs.finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("done")))
            .await?;
    }
    Ok(())
}
```

## Subject Registry and Consent Stores

Every session backend ships the `adk-core` subject registry and consent store, so
`SubjectRights` runs on the database a deployment already uses:

| Backend | Feature | Subject registry | Consent store |
|---------|---------|------------------|---------------|
| In-memory (`adk-core`) | — | `InMemorySubjectRegistry` | `InMemoryConsentStore` |
| SQLite | `sqlite` | `SqliteSubjectRegistry` | `SqliteConsentStore` |
| PostgreSQL | `postgres` | `PostgresSubjectRegistry` | `PostgresConsentStore` |
| Redis | `redis` | `RedisSubjectRegistry` | `RedisConsentStore` |
| MongoDB | `mongodb` | `MongoSubjectRegistry` | `MongoConsentStore` |
| Neo4j | `neo4j` | `Neo4jSubjectRegistry` | `Neo4jConsentStore` |
| Firestore | `firestore` | `FirestoreSubjectRegistry` | `FirestoreConsentStore` |

Each type has `new(...)`, a constructor over a shared handle, and `migrate()`; the registries
add `with_clock`. A visitor id links once (a conditional write on the empty link), and a
revocation keeps its first time. `SessionSubjectData` is the session family's
`SubjectDataStore` adapter over any `SessionService`: it exports a subject's sessions with their
events and `user:` state, and erases them with `delete_all_sessions`.

```rust
use std::sync::Arc;

use adk_core::subject::SubjectRights;
use adk_session::{SessionSubjectData, SqliteConsentStore, SqliteSessionService, SqliteSubjectRegistry};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let url = "sqlite://app.db?mode=rwc";
    let sessions = SqliteSessionService::new(url).await?;
    sessions.migrate().await?;
    let sessions = Arc::new(sessions);
    let registry = SqliteSubjectRegistry::new(url).await?;
    registry.migrate().await?;
    let consent = SqliteConsentStore::new(url).await?;
    let rights = SubjectRights::new(Arc::new(registry), Arc::new(consent))
        .with_app("shop")
        .with_store(Arc::new(SessionSubjectData::new(sessions)));
    let report = rights.erase(&adk_core::SubjectRef::human("alice")).await?;
    println!("{:?}", report.status);
    Ok(())
}
```

## Consent-Gated Sessions

`ConsentGatedSessionService` wraps any `SessionService` with an `adk_core::subject::ConsentGate`.
For a gated user (a visitor id by default), sessions reach the durable service only with consent
for `session_continuity`, and `user:` keys only with consent for `persistent_profile`; without
it a session lives in process memory and the keys are dropped. A subject being erased can write
nothing. See [Subject Rights](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/security/subject-rights.md#consent-gates).

## Rewind

`rewind(&identity, event_id)` removes the events after `event_id`, and `rewind_steps(&identity, n)` removes the last `n`. Both address the session by its full `AdkIdentity` — app, user, and session — and rebuild the session-scoped state from the state the session was created with plus the remaining events' deltas; app and user state are shared with other sessions and are not rewound. The in-memory and SQLite backends and `EncryptedSession` implement rewind; every other backend returns `session.rewind.unsupported`.

```rust
use adk_core::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{InMemorySessionService, SessionService};

async fn undo_last_turn(service: &InMemorySessionService) -> adk_core::Result<()> {
    let identity = AdkIdentity::new(
        AppName::try_from("my_app")?,
        UserId::try_from("user_123")?,
        SessionId::try_from("session_456")?,
    );
    let _session = service.rewind_steps(&identity, 2).await?;
    Ok(())
}
```

| Error | Category |
|-------|----------|
| `session.rewind.unsupported` | `Unsupported` |
| `session.not_found` | `NotFound` |
| `session.event_not_found` | `NotFound` |
| `session.rewind.too_many_steps` | `InvalidInput` |

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `rewind` and `rewind_steps` take `&AdkIdentity` instead of a session ID, and keep the session's initial state | Build an `AdkIdentity` |
| `create` for an existing identity fails with `session.already_exists` on every backend | Handle `Conflict` |
| Appending to a missing session fails with `session.not_found` | Match the typed error |
| Every method on every backend validates identifiers | — |
| `get` and `list` read the current `app:` and `user:` tiers | — |
| `delete_all_sessions` also erases the user's `user:` tier; Vertex AI implements it; the default returns `session.delete_all.unsupported` | Keep a value that must outlive the user's sessions in `app:` state or outside the session store |
| Concurrent appends keep every state delta; equal timestamps keep append order | — |
| SQL backends use SQLx 0.9; `sqlx` and `pgvector` are no longer implicit features | Depend on SQLx 0.9 when sharing a pool; enable `sqlite` or `postgres` |
| `sql_schema_version` and `schema_version()` return read failures as errors | — |
| Migration runners take `&'static` steps | — |
| Durable stores, durable runtime stores, and audit log stores on every backend | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Session` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Uses sessions for execution

## SQLx compatibility

ADK-Rust 3.0 SQL backends use SQLx 0.9. SQLx types appear in this crate's public
API — `from_pool` on every SQL store and session service, `SqliteSessionService::pool`, and the `migration` runners — so an application that passes in its own pool must depend on
SQLx 0.9 too. Applications that let ADK open its own connections need no SQLx
dependency. The [SQLx 0.9 migration guide](../docs/official_docs/migration/sqlx-0.9.md)
lists who is affected and what changed, and its
[TLS behaviour](../docs/official_docs/migration/sqlx-0.9.md#tls-behaviour) section
covers PostgreSQL certificate verification.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
