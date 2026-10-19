- **SQLx 0.9 database APIs** (`adk-session`, `adk-memory`, `adk-graph`,
  `adk-auth`, `adk-rag`): applications sharing pools with `from_pool`, `pool()`
  accessors or the migration runners must update their direct SQLx dependency to
  0.9. PostgreSQL backends use Tokio and rustls with aws-lc-rs and negotiate TLS
  under the default `sslmode=prefer` without verifying the certificate; use
  `sslmode=verify-full` to verify it. Replace removed combined runtime/TLS flags in
  application manifests. Memory's pgvector integration moves to 0.4.2. See
  `docs/official_docs/migration/sqlx-0.9.md`.
- **Migration registry identifiers are validated** (`adk-session`, `adk-memory`): the
  public SQL migration runners refuse a registry table name that is not an ASCII
  identifier of at most 63 bytes, starting with a letter or underscore and followed by
  letters, digits or underscores. Step descriptions are bound as parameters, so they
  may contain apostrophes.
- **No implicit `sqlx` and `pgvector` features** (`adk-session`, `adk-memory`,
  `adk-graph`): the optional dependencies are enabled only through `sqlite` and
  `postgres` (`adk-session`, `adk-graph`) and `sqlite-memory`, `graph-memory` and
  `database-memory` (`adk-memory`). Enable those instead of `sqlx` or `pgvector`.
