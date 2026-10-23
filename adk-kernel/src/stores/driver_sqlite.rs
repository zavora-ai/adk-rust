//! The SQLite [`DriverStore`](crate::drivers::DriverStore) (spec 3.2 §7.7, §13 Q8; features
//! `drivers`, `sqlite`).

use super::driver_sql::{REGISTRY_TABLE, sql_driver_store};
use crate::error::{KernelError, Result};

/// Migrations of [`SqliteDriverStore`], recorded in `_adk_kernel_driver_migrations`.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel driver and parked-call tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_drivers (\
    id TEXT PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_driver_parks (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    call_id TEXT NOT NULL, \
    driver_id TEXT NOT NULL, \
    park TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, call_id)\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_driver_parks_driver ON adk_kernel_driver_parks (driver_id);",
)];

/// A [`DriverStore`](crate::drivers::DriverStore) in SQLite (spec 3.2 §7.7, features
/// `drivers`, `sqlite`).
///
/// In the CLI it lives in `harness.db` on the same pool as the harness and process stores:
/// its tables are `adk_kernel_drivers` and `adk_kernel_driver_parks`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_driver_migrations`. Call
/// `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverRecord, DriverStore};
/// use adk_kernel::stores::SqliteDriverStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteDriverStore::new(pool);
/// store.migrate().await?;
/// let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n")?;
/// store.put(DriverRecord::new(manifest, "alice", chrono::Utc::now())).await?;
/// assert!(store.get("fs").await?.is_some());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteDriverStore {
    pool: sqlx::SqlitePool,
}

impl SqliteDriverStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's tables. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            REGISTRY_TABLE,
            MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

sql_driver_store!(SqliteDriverStore, "sqlite");
