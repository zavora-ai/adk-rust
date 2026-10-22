//! The SQLite and PostgreSQL [`AgendaStore`](crate::AgendaStore)s (spec 3.2 §7.5, §13 Q8).
//!
//! One row per item in `adk_kernel_agenda`: the columns a filter or the order reads
//! (`partition_id`, `state`, `created_at` in microseconds since the Unix epoch) beside the
//! item as JSON text, so an item reads back exactly as written. `transition` is one
//! `UPDATE … WHERE id = … AND state = …`, so two writers moving one item from the same
//! state move it once.

use crate::agenda::{AgendaFilter, AgendaItem};
use crate::error::{KernelError, Result};

/// The migration registry of the agenda store, distinct from every other store's.
pub(crate) const AGENDA_REGISTRY_TABLE: &str = "_adk_kernel_agenda_migrations";

/// The SQLite schema.
#[cfg(feature = "sqlite")]
const SQLITE_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel agenda table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_agenda (\
    id TEXT PRIMARY KEY, \
    partition_id TEXT NOT NULL, \
    state TEXT NOT NULL, \
    created_at BIGINT NOT NULL, \
    item TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_agenda_partition ON adk_kernel_agenda (partition_id, state, created_at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_agenda_state ON adk_kernel_agenda (state, created_at, id);",
)];

/// The PostgreSQL schema; `id` takes the `C` collation, so ties order by the bytes of the
/// UTF-8 text on every server, as the other backends do.
#[cfg(feature = "postgres")]
const POSTGRES_MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create the kernel agenda table",
    "\
CREATE TABLE IF NOT EXISTS adk_kernel_agenda (\
    id TEXT COLLATE \"C\" PRIMARY KEY, \
    partition_id TEXT NOT NULL, \
    state TEXT NOT NULL, \
    created_at BIGINT NOT NULL, \
    item TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_kernel_agenda_partition ON adk_kernel_agenda (partition_id, state, created_at, id);\
CREATE INDEX IF NOT EXISTS adk_kernel_agenda_state ON adk_kernel_agenda (state, created_at, id);",
)];

pub(crate) const PUT: &str = "\
INSERT INTO adk_kernel_agenda (id, partition_id, state, created_at, item) \
VALUES ($1, $2, $3, $4, $5) \
ON CONFLICT (id) DO UPDATE SET \
    partition_id = excluded.partition_id, state = excluded.state, \
    created_at = excluded.created_at, item = excluded.item";

pub(crate) const GET: &str = "SELECT item FROM adk_kernel_agenda WHERE id = $1";

pub(crate) const TRANSITION: &str = "\
UPDATE adk_kernel_agenda SET partition_id = $1, state = $2, created_at = $3, item = $4 \
WHERE id = $5 AND state = $6";

pub(crate) const DELETE: &str = "DELETE FROM adk_kernel_agenda WHERE id = $1";

/// The stored order key of an item: microseconds since the Unix epoch.
pub(crate) fn created_at(item: &AgendaItem) -> i64 {
    item.created_at.timestamp_micros()
}

/// The `list` statement for `filter` and its text and limit parameters, in order.
pub(crate) fn list(filter: &AgendaFilter) -> (String, Vec<String>, Option<i64>) {
    let mut conditions = Vec::new();
    let mut binds = Vec::new();
    if let Some(partition) = &filter.partition {
        binds.push(partition.as_str().to_string());
        conditions.push(format!("partition_id = ${}", binds.len()));
    }
    if !filter.states.is_empty() {
        let mut placeholders = Vec::new();
        for state in &filter.states {
            binds.push(state.as_str().to_string());
            placeholders.push(format!("${}", binds.len()));
        }
        conditions.push(format!("state IN ({})", placeholders.join(", ")));
    }
    let mut sql = "SELECT item FROM adk_kernel_agenda".to_string();
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at, id");
    let limit = filter.limit.map(|limit| i64::try_from(limit).unwrap_or(i64::MAX));
    if limit.is_some() {
        sql.push_str(&format!(" LIMIT ${}", binds.len() + 1));
    }
    (sql, binds, limit)
}

/// Reads a stored item back.
pub(crate) fn decode(backend: &'static str, item: &str) -> Result<AgendaItem> {
    serde_json::from_str(item).map_err(|error| KernelError::Store {
        backend,
        reason: format!("an agenda row is corrupt: {error}"),
    })
}

/// Maps a database error.
pub(crate) fn db_error(backend: &'static str, action: &str, error: &sqlx::Error) -> KernelError {
    KernelError::Store { backend, reason: format!("could not {action} the agenda table: {error}") }
}

/// Implements [`AgendaStore`](crate::AgendaStore) for a store with a `pool` field over the
/// shared statements.
macro_rules! sql_agenda_store {
    ($store:ty, $backend:literal) => {
        #[async_trait::async_trait]
        impl $crate::agenda::AgendaStore for $store {
            async fn put(&self, item: $crate::agenda::AgendaItem) -> $crate::error::Result<()> {
                let json = serde_json::to_string(&item)?;
                sqlx::query($crate::stores::agenda_sql::PUT)
                    .bind(&item.id)
                    .bind(item.partition.as_str())
                    .bind(item.state.kind().as_str())
                    .bind($crate::stores::agenda_sql::created_at(&item))
                    .bind(json)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agenda_sql::db_error($backend, "write", &e))?;
                Ok(())
            }

            async fn get(
                &self,
                id: &str,
            ) -> $crate::error::Result<Option<$crate::agenda::AgendaItem>> {
                let row: Option<(String,)> = sqlx::query_as($crate::stores::agenda_sql::GET)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agenda_sql::db_error($backend, "read", &e))?;
                row.map(|(item,)| $crate::stores::agenda_sql::decode($backend, &item)).transpose()
            }

            async fn list(
                &self,
                filter: &$crate::agenda::AgendaFilter,
            ) -> $crate::error::Result<Vec<$crate::agenda::AgendaItem>> {
                let (sql, binds, limit) = $crate::stores::agenda_sql::list(filter);
                let mut query = sqlx::query_as::<_, (String,)>(sqlx::AssertSqlSafe(sql));
                for bind in binds {
                    query = query.bind(bind);
                }
                if let Some(limit) = limit {
                    query = query.bind(limit);
                }
                let rows = query
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agenda_sql::db_error($backend, "list", &e))?;
                rows.iter()
                    .map(|(item,)| $crate::stores::agenda_sql::decode($backend, item))
                    .collect()
            }

            async fn transition(
                &self,
                item: $crate::agenda::AgendaItem,
                from: $crate::agenda::AgendaStateKind,
            ) -> $crate::error::Result<bool> {
                let json = serde_json::to_string(&item)?;
                let done = sqlx::query($crate::stores::agenda_sql::TRANSITION)
                    .bind(item.partition.as_str())
                    .bind(item.state.kind().as_str())
                    .bind($crate::stores::agenda_sql::created_at(&item))
                    .bind(json)
                    .bind(&item.id)
                    .bind(from.as_str())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| $crate::stores::agenda_sql::db_error($backend, "update", &e))?;
                Ok(done.rows_affected() > 0)
            }

            async fn delete(&self, id: &str) -> $crate::error::Result<bool> {
                let done = sqlx::query($crate::stores::agenda_sql::DELETE)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        $crate::stores::agenda_sql::db_error($backend, "delete from", &e)
                    })?;
                Ok(done.rows_affected() > 0)
            }
        }
    };
}

/// An [`AgendaStore`](crate::AgendaStore) in SQLite (spec 3.2 §7.1, §7.5, feature `sqlite`).
///
/// In the CLI it lives in `harness.db` beside the process store, on the same pool: its
/// table is `adk_kernel_agenda`, and [`migrate`](Self::migrate) records its steps in
/// `_adk_kernel_agenda_migrations`. Call `migrate` once before use.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaItem, AgendaOrigin, AgendaStore, SqliteAgendaStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
/// let store = SqliteAgendaStore::new(pool);
/// store.migrate().await?;
/// store.put(AgendaItem::new("item-1", SubjectRef::human("alice"), "Tidy", "Weekly", AgendaOrigin::User, chrono::Utc::now())).await?;
/// assert_eq!(store.get("item-1").await?.unwrap().title, "Tidy");
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteAgendaStore {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteAgendaStore {
    /// A store over `pool`, such as the pool of `harness.db`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// The pool the store writes to.
    pub fn pool(&self) -> &sqlx::SqlitePool {
        &self.pool
    }

    /// Creates or upgrades the store's table; a step already recorded is not run again.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        adk_session::migration::sqlite_runner::run_sql_migrations(
            &self.pool,
            AGENDA_REGISTRY_TABLE,
            SQLITE_MIGRATIONS,
            || async { Ok(false) },
        )
        .await
        .map_err(|error| KernelError::Store { backend: "sqlite", reason: error.message })
    }
}

#[cfg(feature = "sqlite")]
sql_agenda_store!(SqliteAgendaStore, "sqlite");

/// An [`AgendaStore`](crate::AgendaStore) in PostgreSQL, shared by every replica (spec 3.2
/// §7.1, §7.5, feature `postgres`).
///
/// It opens from the URL of `adk-rust serve --store`. Its table is `adk_kernel_agenda`;
/// [`migrate`](Self::migrate) records its steps in `_adk_kernel_agenda_migrations` under an
/// advisory lock, so replicas starting together migrate one at a time.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> adk_kernel::Result<()> {
/// let store = adk_kernel::PostgresAgendaStore::connect("postgres://localhost/app").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "postgres")]
#[derive(Debug, Clone)]
pub struct PostgresAgendaStore {
    pool: sqlx::PgPool,
}

#[cfg(feature = "postgres")]
impl PostgresAgendaStore {
    /// A store over `pool`, such as the one the run store uses.
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

    /// Creates or upgrades the store's table under a session-level advisory lock keyed by
    /// the registry table.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when a statement fails, or when the database was
    /// migrated by a newer release.
    pub async fn migrate(&self) -> Result<()> {
        super::postgres::migrate_locked(
            self.pool.clone(),
            AGENDA_REGISTRY_TABLE,
            POSTGRES_MIGRATIONS,
        )
        .await
    }
}

#[cfg(feature = "postgres")]
sql_agenda_store!(PostgresAgendaStore, "postgres");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agenda::AgendaStateKind;

    #[test]
    fn list_numbers_parameters_in_order() {
        assert_eq!(
            list(&AgendaFilter::default()).0,
            "SELECT item FROM adk_kernel_agenda ORDER BY created_at, id"
        );
        let filter = AgendaFilter::default()
            .partition("work")
            .states([AgendaStateKind::Proposed, AgendaStateKind::Scheduled])
            .limit(5);
        let (sql, binds, limit) = list(&filter);
        assert_eq!(
            sql,
            "SELECT item FROM adk_kernel_agenda WHERE partition_id = $1 AND state IN ($2, $3) \
             ORDER BY created_at, id LIMIT $4"
        );
        assert_eq!(binds, ["work", "proposed", "scheduled"]);
        assert_eq!(limit, Some(5));
    }
}
