# Sharing SQLx pools with ADK-Rust 3.0

This guide covers the move from SQLx 0.8, used by ADK-Rust 2.x, to SQLx 0.9, used by ADK-Rust 3.0.
It is part of the [2.x → 3.0 migration](2.x-to-3.0.md).

ADK-Rust 3.0 database backends use SQLx 0.9. SQLx 0.8 and 0.9 types are distinct
Rust types, so an application that passes its own pool into an ADK constructor must
depend on SQLx 0.9. An application that lets ADK open its connections from a URL
needs no SQLx dependency at all.

## Who is affected

| Your application | What to do |
|------------------|------------|
| Uses no SQL backend (in-memory, Redis, MongoDB, Firestore, Neo4j, Vertex AI) | Nothing. SQLx is not compiled. |
| Uses SQLite backends, constructed from a URL (`new`) | Nothing. |
| Uses PostgreSQL backends without pgvector, constructed from a URL | Nothing for the code. Connections now negotiate TLS when the server offers it; read [TLS behaviour](#tls-behaviour). |
| Uses pgvector (`adk-memory` `database-memory`, `adk-rag` `pgvector`) | Check collection names against the [pgvector naming rule](#pgvector-collection-names); `PgVectorStore::new` now returns `RagError`. |
| Shares its own SQLx pool with ADK (`from_pool`, the `migration` runners) | Move your `sqlx` dependency to 0.9 and apply the [SQLx 0.9 changes](#changes-in-sqlx-09) to your own queries. |
| Matches on `GraphError::DatabaseError` or calls `sql_schema_version` | See [API changes](#api-changes). |

Persisted database schemas do not change. Existing migrations and checkpoint data
continue through the existing backend migration paths.

## Sharing a pool

Depend on SQLx 0.9 with the runtime and database you use. ADK's own features add the
rest, including TLS for PostgreSQL, through Cargo feature unification.

```toml
[dependencies]
sqlx = { version = "0.9", default-features = false, features = ["runtime-tokio", "sqlite"] }
adk-graph = { version = "3.0.0", features = ["sqlite"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use adk_graph::checkpoint::SqliteCheckpointer;
use sqlx::SqlitePool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pool = SqlitePool::connect("sqlite::memory:").await?;
    let checkpointer = SqliteCheckpointer::from_pool(pool.clone()).await?;
    // `pool` and `checkpointer` address the same database.
    drop(checkpointer);
    Ok(())
}
```

For PostgreSQL, enable `postgres` and `runtime-tokio`. SQLx 0.9 removed the combined
runtime and TLS flags such as `runtime-tokio-rustls`; replace them with
`runtime-tokio` plus a separate TLS flag only if you need a specific TLS stack (see
[TLS behaviour](#tls-behaviour)).

## Public SQLx surface

These items take or return SQLx types, so they follow the SQLx version of the release.
SQLx is a public dependency of these crates (see `STABILITY.md`, "Public Dependency
Policy").

| Crate | Features | Items |
|-------|----------|-------|
| `adk-session` | `sqlite`, `postgres` | `from_pool` on `SqliteSessionService`, `PostgresSessionService`, and every SQL store (`*RunStore`, `*ScheduleStore`, `*LeaseStore`, `*GovernanceStateStore`, `*ActionLedger`, `*ApprovalStore`, `*SpendLedger`, `*AuditLogStore`); `SqliteSessionService::pool`; `migration::sqlite_runner` and `migration::pg_runner` (`run_sql_migrations`, `run_sql_migrations_on_connection`, `sql_schema_version`) |
| `adk-memory` | `sqlite-memory`, `graph-memory`, `database-memory` | `SqliteMemoryService::from_pool`, `GraphMemoryService::from_pool`, `PostgresMemoryService::from_pool` and `from_pool_with_index`; the `migration` runners |
| `adk-graph` | `sqlite`, `postgres` | `SqliteCheckpointer::from_pool`, `PostgresCheckpointer::from_pool` |
| `adk-auth` | `postgres-audit` | `PostgresAuditSink::from_pool`, `PostgresAuditSink::pool` |
| `adk-rag` | `pgvector` | `PgVectorStore::from_pool` |

No public error type carries an SQLx error: database failures are reported as each
crate's own error with the SQLx message as text.

## API changes

| Item | 2.x | 3.0 |
|------|-----|-----|
| `GraphError::DatabaseError(sqlx::Error)` | Present only with the `sqlite` feature, never constructed | Removed. Checkpointer database failures are `GraphError::CheckpointError`, as they already were. |
| `PgVectorStore::new` | `Result<Self, sqlx::Error>` | `adk_rag::Result<Self>`; a connection failure is `RagError::VectorStoreError` |
| `sql_schema_version`, `schema_version()` on SQL services | Every error read as version 0 | 0 only when the registry table does not exist; any other failure (lost connection, missing privilege, misshapen table) is returned as an error |
| Migration registry table names | ASCII identifier | ASCII identifier of at most 63 bytes, starting with a letter or underscore |
| pgvector collection names | Rewritten to fit | Validated; see [below](#pgvector-collection-names) |
| `AuditFilter::resource` on `PostgresAuditSink` | `LIKE` pattern: `%` and `_` were wildcards | Literal substring, as in the in-memory filter |
| `SqliteCheckpointer` schema upgrade | Every `ALTER TABLE` error ignored | Only the duplicate-column error is ignored; any other failure is returned |

The public session and memory migration runners accept registry table names of at
most 63 bytes containing ASCII letters, digits and underscores, starting with a letter
or underscore. Schema-qualified identifiers are not supported. Descriptions may contain
quotes and are bound as data. Migration SQL bodies are trusted executable SQL: do not
construct them from untrusted application input.

## pgvector collection names

`PgVectorStore` stores each collection in the table `rag_<name>`. A collection name
must be 1 to 59 bytes of ASCII lowercase letters, digits and underscores, so the table
name fits PostgreSQL's 63-byte identifier limit. Any other name returns
`RagError::VectorStoreError`.

2.x replaced every other character with `_`, and PostgreSQL folded the result to
lowercase and truncated it at 63 bytes, so `docs-v1`, `docs v1`, `docs_v1` and
`Docs_V1` all addressed the table `rag_docs_v1`. To keep reading an existing
collection, use the name of the table it was stored in, without the `rag_` prefix:
`docs-v1` becomes `docs_v1`. A collection whose name contained non-ASCII letters has to
be renamed in the database (`ALTER TABLE ... RENAME TO rag_<new_name>`).

`PostgresMemoryService` (`adk-memory`) is unaffected: it uses one fixed table.

## TLS behaviour

PostgreSQL features (`adk-session` `postgres`, `adk-memory` `database-memory`,
`adk-graph` `postgres`, `adk-auth` `postgres-audit`, `adk-rag` `pgvector`) now always
compile SQLx with rustls and the aws-lc-rs crypto provider. A standalone PostgreSQL
backend no longer depends on another ADK crate enabling TLS.

| `sslmode` | Behaviour |
|-----------|-----------|
| `disable` | No TLS. |
| `prefer` (default when the URL sets none) | TLS when the server offers it, plaintext otherwise. The certificate is **not** verified. |
| `require` | TLS or fail. The certificate is **not** verified. |
| `verify-ca` | TLS; the certificate chain is verified, the host name is not. |
| `verify-full` | TLS; chain and host name are verified. |

- **Encryption is not authentication.** Under the default `prefer`, a server that
  supports TLS now gets an encrypted connection, but an attacker who can intercept the
  connection can still impersonate the server. Use `sslmode=verify-full` in production.
- **Trust roots.** Verification uses the bundled Mozilla root set (`webpki-roots`), not
  the operating system's certificate store. For a private CA, such as Amazon RDS or a
  corporate CA, pass its certificate with `sslrootcert=<file>`:
  `postgres://app@db.internal/app?sslmode=verify-full&sslrootcert=/etc/ssl/rds-ca.pem`.
- **Feature unification.** SQLx is one crate per build. An application that enables
  `tls-native-tls`, `tls-rustls-ring-webpki` or `tls-rustls-ring-native-roots` on its
  own `sqlx` dependency changes the TLS stack, or the root store, used by ADK's pools
  too. Enable a TLS flag on your own `sqlx` dependency only when you want that effect.

## Changes in SQLx 0.9

These come from SQLx itself and are visible through ADK or in code that shares a pool.

- **PostgreSQL error text.** A PostgreSQL error message now ends with ` at line N`,
  the line in the server's C source that raised it. ADK error messages that include
  the database message carry the suffix; match on SQLSTATE codes, not message text.
- **`.pgpass`.** Backslash escapes in the password field are now processed, which can
  change the password sent for a password containing `\`. On Windows the file is read
  from `%APPDATA%\postgresql\pgpass.conf`.
- **Connection `options`.** Values passed to `PgConnectOptions::options()` or the
  `options` URL parameter are now escaped by SQLx. Remove any manual escaping.
- **Query strings.** `query*()` functions take `impl SqlSafeStr`, implemented for
  `&'static str` and `AssertSqlSafe`. Wrap a query built at runtime in
  `sqlx::AssertSqlSafe(...)` after making sure it holds no untrusted input. See the
  [SQLx 0.9 changelog](https://github.com/launchbadge/sqlx/blob/main/CHANGELOG.md)
  for the full list.

SQLite telemetry (`adk-telemetry`) remains compatible with the native SQLite library
used by SQLx.
