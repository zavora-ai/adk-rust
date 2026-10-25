//! The PostgreSQL [`ControlStore`](super::ControlStore) (spec 3.3 §10 K3, K4; feature
//! `postgres`).

use super::sql::{REGISTRY_TABLE, sql_control_store};
use crate::error::{KernelError, Result};

/// Migrations of [`PostgresControlStore`], recorded in `_adk_kernel_control_migrations`.
///
/// The key columns take the `C` collation, so lists order by the bytes of the UTF-8 text on
/// every server, as the other backends do.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create kernel area settings and away cursor tables",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_area_settings (\
    partition_id TEXT COLLATE \"C\" PRIMARY KEY, \
    record TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS adk_kernel_away_cursors (\
    user_id TEXT COLLATE \"C\" NOT NULL, \
    viewer TEXT COLLATE \"C\" NOT NULL, \
    seq BIGINT NOT NULL, \
    record TEXT NOT NULL, \
    PRIMARY KEY (user_id, viewer)\
);",
)];

/// A [`ControlStore`](super::ControlStore) in PostgreSQL, shared by every replica (spec 3.3
/// §10 K3, K4; feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its tables are
/// `adk_kernel_area_settings` and `adk_kernel_away_cursors`; [`migrate`](Self::migrate)
/// records its steps in `_adk_kernel_control_migrations` under an advisory lock, so replicas
/// starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::control::PostgresControlStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresControlStore {
    pool: sqlx::PgPool,
}

impl PostgresControlStore {
    /// A store over `pool`, such as the one the process store uses.
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
        crate::pairing::postgres::migrate_locked(self.pool.clone(), REGISTRY_TABLE, MIGRATIONS)
            .await
    }
}

sql_control_store!(PostgresControlStore, "postgres");
