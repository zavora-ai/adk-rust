//! The PostgreSQL [`DriverStore`](crate::drivers::DriverStore) (spec 3.2 §7.7, §13 Q8;
//! features `drivers`, `postgres`).

use super::driver_sql::{REGISTRY_TABLE, sql_driver_store};
use crate::error::{KernelError, Result};

/// Migrations of [`PostgresDriverStore`], recorded in `_adk_kernel_driver_migrations`.
///
/// The key columns take the `C` collation, so lists order by the bytes of the UTF-8 text on
/// every server, as the other backends do.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel driver and parked-call tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_drivers (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_driver_parks (\
    app_name TEXT COLLATE \"C\" NOT NULL, \
    user_id TEXT COLLATE \"C\" NOT NULL, \
    session_id TEXT COLLATE \"C\" NOT NULL, \
    call_id TEXT COLLATE \"C\" NOT NULL, \
    driver_id TEXT NOT NULL, \
    park TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, call_id)\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_driver_parks_driver ON adk_kernel_driver_parks (driver_id);",
)];

/// A [`DriverStore`](crate::drivers::DriverStore) in PostgreSQL, shared by every replica
/// (spec 3.2 §7.7, features `drivers`, `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its tables are `adk_kernel_drivers`
/// and `adk_kernel_driver_parks`; [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_driver_migrations` under an advisory lock, so replicas starting together
/// migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::stores::PostgresDriverStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresDriverStore {
    pool: sqlx::PgPool,
}

impl PostgresDriverStore {
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
        migrate_locked(self.pool.clone()).await
    }
}

/// Runs the migrations under the advisory lock. Boxed so the future is checked for `Send`
/// here rather than inside callers' `async_trait` methods.
fn migrate_locked(
    pool: sqlx::PgPool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use sqlx::Connection as _;

    let store_error = |action: &str, error: &dyn std::fmt::Display| KernelError::Store {
        backend: "postgres",
        reason: format!("could not {action}: {error}"),
    };
    Box::pin(async move {
        let key = REGISTRY_TABLE
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
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "postgres", reason: error.message });
        let _ = lock.close().await;
        result
    })
}

sql_driver_store!(PostgresDriverStore, "postgres");
