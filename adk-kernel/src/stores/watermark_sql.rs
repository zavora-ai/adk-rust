//! The SQLite and PostgreSQL [`CronWatermarkStore`](crate::CronWatermarkStore)s (spec 3.2
//! §7.4, §13 Q8).
//!
//! One row per cron source in `adk_kernel_cron_watermarks`: the source's name and its cursor
//! in microseconds since the Unix epoch. An advance is one upsert that keeps the later
//! cursor, so replicas writing in any order never move a cursor back.

use crate::error::{KernelError, Result};

/// The migration registry of the cron watermark store, distinct from every other store's.
pub(crate) const WATERMARK_REGISTRY_TABLE: &str = "_adk_kernel_cron_watermark_migrations";

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel cron watermark table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_cron_watermarks (\
    source TEXT PRIMARY KEY, \
    cursor_us BIGINT NOT NULL\
);",
)];

/// The PostgreSQL schema.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel cron watermark table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_cron_watermarks (\
    source TEXT COLLATE \"C\" PRIMARY KEY, \
    cursor_us BIGINT NOT NULL\
);",
)];

pub(crate) const READ: &str = "SELECT cursor_us FROM adk_kernel_cron_watermarks WHERE source = $1";

/// SQLite's two-argument `MAX` is its scalar maximum.
#[cfg(feature = "sqlite")]
pub(crate) const SQLITE_ADVANCE: &str = "\
INSERT INTO adk_kernel_cron_watermarks (source, cursor_us) VALUES ($1, $2) \
ON CONFLICT (source) DO UPDATE SET \
cursor_us = MAX(adk_kernel_cron_watermarks.cursor_us, excluded.cursor_us) \
RETURNING cursor_us";

#[cfg(feature = "postgres")]
pub(crate) const POSTGRES_ADVANCE: &str = "\
INSERT INTO adk_kernel_cron_watermarks (source, cursor_us) VALUES ($1, $2) \
ON CONFLICT (source) DO UPDATE SET \
cursor_us = GREATEST(adk_kernel_cron_watermarks.cursor_us, excluded.cursor_us) \
RETURNING cursor_us";

pub(crate) const DELETE: &str = "DELETE FROM adk_kernel_cron_watermarks WHERE source = $1";

/// The instant `micros` microseconds after the Unix epoch.
pub(crate) fn instant(backend: &'static str, micros: i64) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp_micros(micros).ok_or_else(|| KernelError::Store {
        backend,
        reason: format!("a stored cron watermark is out of range: {micros}"),
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store {
        backend,
        reason: format!("could not {action} the cron watermarks: {error}"),
    }
}

/// Implements [`CronWatermarkStore`](crate::CronWatermarkStore) for a store with a `pool`
/// field, advancing with `$advance`.
macro_rules! sql_watermark_store {
    ($store:ty, $backend:literal, $advance:expr) => {
        #[async_trait::async_trait]
        impl $crate::watermark::CronWatermarkStore for $store {
            async fn read(
                &self,
                source: &str,
            ) -> $crate::error::Result<Option<chrono::DateTime<chrono::Utc>>> {
                let row: Option<(i64,)> = sqlx::query_as($crate::stores::watermark_sql::READ)
                    .bind(source)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::watermark_sql::db_error($backend, "read", &e))?;
                row.map(|(micros,)| $crate::stores::watermark_sql::instant($backend, micros))
                    .transpose()
            }

            async fn advance(
                &self,
                source: &str,
                cursor: chrono::DateTime<chrono::Utc>,
            ) -> $crate::error::Result<chrono::DateTime<chrono::Utc>> {
                let (micros,): (i64,) = sqlx::query_as($advance)
                    .bind(source)
                    .bind(cursor.timestamp_micros())
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::watermark_sql::db_error($backend, "advance", &e)
                    })?;
                $crate::stores::watermark_sql::instant($backend, micros)
            }

            async fn delete(&self, source: &str) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::watermark_sql::DELETE)
                    .bind(source)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::watermark_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }
        }
    };
}

/// A [`CronWatermarkStore`](crate::CronWatermarkStore) in SQLite (spec 3.2 §7.4, feature
/// `sqlite`).
///
/// Its table is `adk_kernel_cron_watermarks`, and [`migrate`](Self::migrate) records its
/// steps in `_adk_kernel_cron_watermark_migrations`, distinct from every other store's, so
/// it shares a pool such as `harness.db`'s. Call `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{CronWatermarkStore, SqliteCronWatermarkStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteCronWatermarkStore::new(pool);
/// store.migrate().await?;
/// let now = chrono::Utc::now();
/// store.advance("nightly", now).await?;
/// assert_eq!(store.read("nightly").await?.map(|at| at.timestamp_micros()), Some(now.timestamp_micros()));
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteCronWatermarkStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteCronWatermarkStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's table.
    ///
    /// Idempotent: a step already recorded in `_adk_kernel_cron_watermark_migrations` is
    /// not run again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            WATERMARK_REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

#[cfg(feature = "sqlite")]
sql_watermark_store!(SqliteCronWatermarkStore, "sqlite", SQLITE_ADVANCE);

/// A [`CronWatermarkStore`](crate::CronWatermarkStore) in PostgreSQL, shared by every
/// replica (spec 3.2 §7.4, feature `postgres`).
///
/// Its table is `adk_kernel_cron_watermarks`; [`migrate`](Self::migrate) records its steps
/// in `_adk_kernel_cron_watermark_migrations` under an advisory lock, so replicas starting
/// together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::PostgresCronWatermarkStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresCronWatermarkStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresCronWatermarkStore {
    /// A store over `pool`, such as the one the session service and the run store use.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// Connects to `database_url` with a pool of its own.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the server cannot be reached.
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool =
            sqlx::PgPool::connect(database_url).await.map_err(|error| KernelError::Store {
                backend: "postgres",
                reason: format!("could not connect: {error}"),
            })?;
        Ok(Self::new(pool))
    }

    /// Creates or upgrades the store's table, under a session-level advisory lock keyed by
    /// the registry table, so replicas starting together migrate one at a time.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        migrate_locked(self.pool.clone()).await
    }
}

/// Runs the migrations under the advisory lock. Boxed so the future is checked for `Send`
/// here, with concrete lifetimes, rather than inside callers' `async_trait` methods.
#[cfg(feature = "postgres")]
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = WATERMARK_REGISTRY_TABLE
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            })
            .cast_signed();
        let mut lock = sqlx::PgConnection::connect_with(&pool.connect_options())
            .await
            .map_err(|error| store_error("connect for the migration lock", &error))?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut lock)
            .await
            .map_err(|error| store_error("take the migration lock", &error))?;
        let result = adk_session::migration::pg_runner::run_sql_migrations(
            &pool,
            WATERMARK_REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

#[cfg(feature = "postgres")]
sql_watermark_store!(PostgresCronWatermarkStore, "postgres", POSTGRES_ADVANCE);
