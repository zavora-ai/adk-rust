//! SQL-backed [`AuditLogStore`](adk_core::audit_log::AuditLogStore) implementations.
//!
//! | Table | Key | Holds |
//! |-------|-----|-------|
//! | `adk_audit_logs` | log id | The log's size, the revision every append compares against |
//! | `adk_audit_entries` | log id, seq | Leaf hash and canonical record; the record is `NULL` once pruned |
//! | `adk_audit_nodes` | log id, level, index | Hashes of complete Merkle subtrees |
//! | `adk_audit_checkpoints` | log id, size | Root hash and signed note |
//!
//! | Backend | Feature | How two writers on one log are serialized |
//! |---------|---------|------------------------------------------|
//! | [`SqliteAuditLogStore`](crate::SqliteAuditLogStore) | `sqlite` | `BEGIN IMMEDIATE` takes the write lock before the size is read |
//! | [`PostgresAuditLogStore`](crate::PostgresAuditLogStore) | `postgres` | `SELECT ... FOR UPDATE` on the log's size row |

use std::ops::Range;

use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, out_of_range_error,
    validate_append, validate_checkpoint,
};
use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::DateTime;
use sqlx::Row;

const REGISTRY_TABLE: &str = "_adk_audit_log_migrations";

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create audit log tables",
    "\
CREATE TABLE IF NOT EXISTS adk_audit_logs (\
    log_id TEXT PRIMARY KEY, \
    size BIGINT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_audit_entries (\
    log_id TEXT NOT NULL, \
    seq BIGINT NOT NULL, \
    leaf_hash TEXT NOT NULL, \
    record TEXT, \
    PRIMARY KEY (log_id, seq)\
);\
CREATE TABLE IF NOT EXISTS adk_audit_nodes (\
    log_id TEXT NOT NULL, \
    level INTEGER NOT NULL, \
    idx BIGINT NOT NULL, \
    hash TEXT NOT NULL, \
    PRIMARY KEY (log_id, level, idx)\
);\
CREATE TABLE IF NOT EXISTS adk_audit_checkpoints (\
    log_id TEXT NOT NULL, \
    size BIGINT NOT NULL, \
    root_hash TEXT NOT NULL, \
    note TEXT NOT NULL, \
    created_at_us BIGINT NOT NULL, \
    PRIMARY KEY (log_id, size)\
);",
)];

fn store_error(message: String) -> AdkError {
    AdkError::new(
        adk_core::ErrorComponent::Session,
        adk_core::ErrorCategory::Unavailable,
        "session.audit_log.storage",
        message,
    )
}

fn corrupt(column: &str, error: impl std::fmt::Display) -> AdkError {
    store_error(format!("audit log column '{column}' is corrupt: {error}"))
}

fn to_i64(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| store_error(format!("audit log {what} {value} exceeds i64")))
}

fn to_u64(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).map_err(|e| corrupt(column, e))
}

fn parse_hash(text: &str, column: &str) -> Result<AuditHash> {
    text.parse().map_err(|e| corrupt(column, e))
}

macro_rules! sql_audit_log_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty, begin = $begin:expr, lock = $lock:expr, migrate = $migrate:expr
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: $pool,
        }

        impl $name {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.audit_log.storage` when the database cannot be reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    store_error(format!("audit log database connection failed: {e}"))
                })?;
                Ok(Self { pool })
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool }
            }

            /// Creates the audit log tables if they do not exist.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration step fails.
            pub async fn migrate(&self) -> Result<()> {
                ($migrate)(&self.pool).await
            }
        }

        #[async_trait]
        impl AuditLogStore for $name {
            async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
                let mut tx = self
                    .pool
                    .begin_with($begin)
                    .await
                    .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                sqlx::query(
                    "INSERT INTO adk_audit_logs (log_id, size) VALUES ($1, 0) \
                     ON CONFLICT (log_id) DO NOTHING",
                )
                .bind(&batch.log_id)
                .execute(&mut *tx)
                .await
                .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                let size: i64 = sqlx::query(&format!(
                    "SELECT size FROM adk_audit_logs WHERE log_id = $1{}",
                    $lock
                ))
                .bind(&batch.log_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| store_error(format!("audit log append failed: {e}")))?
                .try_get("size")
                .map_err(|e| corrupt("size", e))?;
                let range = validate_append(&batch, to_u64(size, "size")?)?;
                for entry in &batch.entries {
                    sqlx::query(
                        "INSERT INTO adk_audit_entries (log_id, seq, leaf_hash, record) \
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind(&batch.log_id)
                    .bind(to_i64(entry.seq, "seq")?)
                    .bind(entry.leaf_hash.to_string())
                    .bind(&entry.record)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                }
                for node in &batch.nodes {
                    sqlx::query(
                        "INSERT INTO adk_audit_nodes (log_id, level, idx, hash) \
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind(&batch.log_id)
                    .bind(i32::from(node.level))
                    .bind(to_i64(node.index, "node index")?)
                    .bind(node.hash.to_string())
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                }
                sqlx::query("UPDATE adk_audit_logs SET size = $1 WHERE log_id = $2")
                    .bind(to_i64(range.end, "size")?)
                    .bind(&batch.log_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                tx.commit()
                    .await
                    .map_err(|e| store_error(format!("audit log append failed: {e}")))?;
                Ok(range)
            }

            async fn size(&self, log_id: &str) -> Result<u64> {
                let row = sqlx::query("SELECT size FROM adk_audit_logs WHERE log_id = $1")
                    .bind(log_id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("audit log read failed: {e}")))?;
                match row {
                    Some(row) => to_u64(row.try_get("size").map_err(|e| corrupt("size", e))?, "size"),
                    None => Ok(0),
                }
            }

            async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
                let size = self.size(log_id).await?;
                if range.end > size {
                    return Err(out_of_range_error(log_id, size, range.end));
                }
                if range.start >= range.end {
                    return Ok(Vec::new());
                }
                let rows = sqlx::query(
                    "SELECT seq, leaf_hash, record FROM adk_audit_entries \
                     WHERE log_id = $1 AND seq >= $2 AND seq < $3 ORDER BY seq",
                )
                .bind(log_id)
                .bind(to_i64(range.start, "seq")?)
                .bind(to_i64(range.end, "seq")?)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| store_error(format!("audit log read failed: {e}")))?;
                rows.iter()
                    .map(|row| {
                        let leaf: String = row.try_get("leaf_hash").map_err(|e| corrupt("leaf_hash", e))?;
                        Ok(AuditLogEntry {
                            seq: to_u64(row.try_get("seq").map_err(|e| corrupt("seq", e))?, "seq")?,
                            leaf_hash: parse_hash(&leaf, "leaf_hash")?,
                            record: row.try_get("record").map_err(|e| corrupt("record", e))?,
                        })
                    })
                    .collect()
            }

            async fn hash_at(
                &self,
                log_id: &str,
                level: u8,
                index: u64,
            ) -> Result<Option<AuditHash>> {
                let Ok(index) = i64::try_from(index) else {
                    return Ok(None);
                };
                let row = if level == 0 {
                    sqlx::query(
                        "SELECT leaf_hash AS hash FROM adk_audit_entries \
                         WHERE log_id = $1 AND seq = $2",
                    )
                    .bind(log_id)
                    .bind(index)
                    .fetch_optional(&self.pool)
                    .await
                } else {
                    sqlx::query(
                        "SELECT hash FROM adk_audit_nodes \
                         WHERE log_id = $1 AND level = $2 AND idx = $3",
                    )
                    .bind(log_id)
                    .bind(i32::from(level))
                    .bind(index)
                    .fetch_optional(&self.pool)
                    .await
                }
                .map_err(|e| store_error(format!("audit log read failed: {e}")))?;
                row.map(|row| {
                    let hash: String = row.try_get("hash").map_err(|e| corrupt("hash", e))?;
                    parse_hash(&hash, "hash")
                })
                .transpose()
            }

            async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
                let mut tx = self
                    .pool
                    .begin_with($begin)
                    .await
                    .map_err(|e| store_error(format!("audit checkpoint write failed: {e}")))?;
                let size = sqlx::query(&format!(
                    "SELECT size FROM adk_audit_logs WHERE log_id = $1{}",
                    $lock
                ))
                .bind(&checkpoint.log_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| store_error(format!("audit checkpoint write failed: {e}")))?
                .map(|row| row.try_get::<i64, _>("size").map_err(|e| corrupt("size", e)))
                .transpose()?
                .map_or(Ok(0), |size| to_u64(size, "size"))?;
                let existing = sqlx::query(
                    "SELECT root_hash FROM adk_audit_checkpoints WHERE log_id = $1 AND size = $2",
                )
                .bind(&checkpoint.log_id)
                .bind(to_i64(checkpoint.size, "size")?)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| store_error(format!("audit checkpoint write failed: {e}")))?
                .map(|row| {
                    let root: String =
                        row.try_get("root_hash").map_err(|e| corrupt("root_hash", e))?;
                    parse_hash(&root, "root_hash")
                })
                .transpose()?;
                validate_checkpoint(&checkpoint, size, existing)?;
                sqlx::query(
                    "INSERT INTO adk_audit_checkpoints \
                     (log_id, size, root_hash, note, created_at_us) VALUES ($1, $2, $3, $4, $5) \
                     ON CONFLICT (log_id, size) DO UPDATE SET note = excluded.note, \
                     created_at_us = excluded.created_at_us",
                )
                .bind(&checkpoint.log_id)
                .bind(to_i64(checkpoint.size, "size")?)
                .bind(checkpoint.root_hash.to_string())
                .bind(&checkpoint.note)
                .bind(checkpoint.created_at.timestamp_micros())
                .execute(&mut *tx)
                .await
                .map_err(|e| store_error(format!("audit checkpoint write failed: {e}")))?;
                tx.commit()
                    .await
                    .map_err(|e| store_error(format!("audit checkpoint write failed: {e}")))
            }

            async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
                let rows = sqlx::query(
                    "SELECT log_id, size, root_hash, note, created_at_us FROM adk_audit_checkpoints \
                     WHERE log_id = $1 ORDER BY size DESC LIMIT 1",
                )
                .bind(log_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| store_error(format!("audit checkpoint read failed: {e}")))?;
                Ok(decode_checkpoints(&rows)?.pop())
            }

            async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
                let rows = sqlx::query(
                    "SELECT log_id, size, root_hash, note, created_at_us FROM adk_audit_checkpoints \
                     WHERE log_id = $1 ORDER BY size",
                )
                .bind(log_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| store_error(format!("audit checkpoint read failed: {e}")))?;
                decode_checkpoints(&rows)
            }

            async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
                let size = self.size(log_id).await?;
                if before_seq > size {
                    return Err(out_of_range_error(log_id, size, before_seq));
                }
                let pruned = sqlx::query(
                    "UPDATE adk_audit_entries SET record = NULL \
                     WHERE log_id = $1 AND seq < $2 AND record IS NOT NULL",
                )
                .bind(log_id)
                .bind(to_i64(before_seq, "seq")?)
                .execute(&self.pool)
                .await
                .map_err(|e| store_error(format!("audit log prune failed: {e}")))?;
                Ok(pruned.rows_affected())
            }

            async fn logs(&self) -> Result<Vec<String>> {
                let rows = sqlx::query("SELECT log_id FROM adk_audit_logs ORDER BY log_id")
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("audit log read failed: {e}")))?;
                rows.iter()
                    .map(|row| row.try_get("log_id").map_err(|e| corrupt("log_id", e)))
                    .collect()
            }
        }
    };
}

fn decode_checkpoints<R>(rows: &[R]) -> Result<Vec<AuditCheckpoint>>
where
    R: Row,
    for<'r> &'r str: sqlx::ColumnIndex<R>,
    for<'r> String: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
    for<'r> i64: sqlx::Decode<'r, R::Database> + sqlx::Type<R::Database>,
{
    rows.iter()
        .map(|row| {
            let root: String = row.try_get("root_hash").map_err(|e| corrupt("root_hash", e))?;
            let micros: i64 =
                row.try_get("created_at_us").map_err(|e| corrupt("created_at_us", e))?;
            Ok(AuditCheckpoint {
                log_id: row.try_get("log_id").map_err(|e| corrupt("log_id", e))?,
                size: to_u64(row.try_get("size").map_err(|e| corrupt("size", e))?, "size")?,
                root_hash: parse_hash(&root, "root_hash")?,
                note: row.try_get("note").map_err(|e| corrupt("note", e))?,
                created_at: DateTime::from_timestamp_micros(micros)
                    .ok_or_else(|| corrupt("created_at_us", micros))?,
            })
        })
        .collect()
}

#[cfg(feature = "sqlite")]
sql_audit_log_store! {
    /// SQLite-backed [`AuditLogStore`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_session::SqliteAuditLogStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteAuditLogStore::new("sqlite://audit.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    SqliteAuditLogStore, sqlx::SqlitePool,
    begin = "BEGIN IMMEDIATE",
    lock = "",
    migrate = |pool: &sqlx::SqlitePool| {
        let pool = pool.clone();
        async move {
            crate::migration::sqlite_runner::run_sql_migrations(
                &pool,
                REGISTRY_TABLE,
                MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }
}

#[cfg(feature = "postgres")]
sql_audit_log_store! {
    /// PostgreSQL-backed [`AuditLogStore`]. [`migrate`](Self::migrate) takes an advisory
    /// lock, so concurrent processes can call it at startup.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_session::PostgresAuditLogStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresAuditLogStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// # Ok(())
    /// # }
    /// ```
    PostgresAuditLogStore, sqlx::PgPool,
    begin = "BEGIN",
    lock = " FOR UPDATE",
    migrate = |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
