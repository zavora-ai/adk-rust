//! The PostgreSQL [`DeviceStore`](super::DeviceStore) (spec 3.3 §10 K2; feature
//! `postgres`).

use super::sql::{REGISTRY_TABLE, sql_device_store};
use crate::error::{KernelError, Result};

/// Migrations of [`PostgresDeviceStore`], recorded in `_adk_kernel_device_migrations`.
///
/// The key columns take the `C` collation, so lists order by the bytes of the UTF-8 text on
/// every server, as the other backends do.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel device and pairing-code tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_devices (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    created_us BIGINT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_pairing_codes (\
    code_hash TEXT COLLATE \"C\" PRIMARY KEY, \
    expires_us BIGINT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_pairing_codes_expiry ON adk_kernel_pairing_codes (expires_us);",
)];

/// A [`DeviceStore`](super::DeviceStore) in PostgreSQL, shared by every replica (spec 3.3
/// §10 K2, feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its tables are `adk_kernel_devices`
/// and `adk_kernel_pairing_codes`; [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_device_migrations` under an advisory lock, so replicas starting together
/// migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::pairing::PostgresDeviceStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresDeviceStore {
    pool: sqlx::PgPool,
}

impl PostgresDeviceStore {
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

    /// Creates or upgrades the store's tables, under a session-level advisory lock keyed
    /// by the registry table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        migrate_locked(self.pool.clone(), REGISTRY_TABLE, MIGRATIONS).await
    }
}

/// Runs `migrations` recorded in `registry` under a session-level advisory lock keyed by
/// the registry's name, so replicas starting together migrate one at a time. Boxed so the
/// future is checked for `Send` here rather than inside callers' `async_trait` methods.
pub(crate) fn migrate_locked(
    pool: sqlx::PgPool,
    registry: &'static str,
    migrations: &'static [(i64, &'static str, &'static str)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = registry
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
            registry,
            migrations,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

sql_device_store!(PostgresDeviceStore, "postgres");
