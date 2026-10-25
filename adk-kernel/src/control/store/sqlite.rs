//! The SQLite [`ControlStore`](super::ControlStore) (spec 3.3 §10 K3, K4; feature
//! `sqlite`).

use super::sql::{REGISTRY_TABLE, sql_control_store};
use crate::error::{KernelError, Result};

/// Migrations of [`SqliteControlStore`], recorded in `_adk_kernel_control_migrations`.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel area settings and away cursor tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_area_settings (\
    partition_id TEXT PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_away_cursors (\
    user_id TEXT NOT NULL, \
    viewer TEXT NOT NULL, \
    seq INTEGER NOT NULL, \
    record TEXT NOT NULL, \
    PRIMARY KEY (user_id, viewer)\
);",
)];

/// A [`ControlStore`](super::ControlStore) in SQLite (spec 3.3 §10 K3, K4; feature
/// `sqlite`).
///
/// In the CLI it lives in `harness.db` on the same pool as the harness and process stores:
/// its tables are `adk_kernel_area_settings` and `adk_kernel_away_cursors`, and
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_control_migrations`. Call
/// `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{AwayCursor, ControlStore, SqliteControlStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteControlStore::new(pool);
/// store.migrate().await?;
/// store.advance_cursor(AwayCursor::new("ana", "home", 7, chrono::Utc::now())).await?;
/// assert_eq!(store.cursor("ana", "home").await?.unwrap().seq, 7);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteControlStore {
    pool: sqlx::SqlitePool,
}

impl SqliteControlStore {
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

sql_control_store!(SqliteControlStore, "sqlite");
