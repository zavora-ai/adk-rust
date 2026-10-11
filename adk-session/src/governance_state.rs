//! SQL-backed [`GovernanceStateStore`](adk_core::GovernanceStateStore) implementations.
//!
//! One row per scope in `adk_governance_state`, keyed by [`GovernanceScope::key`](adk_core::GovernanceScope::key).
//!
//! | Change | Statement | Why it has one winner |
//! |--------|-----------|-----------------------|
//! | From epoch 0 | `INSERT ... ON CONFLICT (scope_key) DO NOTHING` | The primary key admits one insert |
//! | From epoch `n` | `UPDATE ... SET epoch = epoch + 1 WHERE scope_key = $k AND epoch = $n` | The epoch is compared inside the write |
//!
//! A write that changes no row reads the current epoch for the `governance.conflict`
//! error's `current_epoch` metadata.
//!
//! | Backend | Feature | Serialization |
//! |---------|---------|---------------|
//! | [`SqliteGovernanceStateStore`](crate::SqliteGovernanceStateStore) | `sqlite` | SQLite's single writer |
//! | [`PostgresGovernanceStateStore`](crate::PostgresGovernanceStateStore) | `postgres` | The row lock the update takes; a waiting update re-checks the epoch against the committed row |

use std::sync::Arc;

use adk_core::governance_state::governance_conflict_error;
use adk_core::principal::SubjectRef;
use adk_core::{
    Clock, GovernanceScope, GovernanceState, GovernanceStateStore, Result, SystemClock,
};

use crate::durable_sql::{from_json, from_micros, now, storage_error, to_json, to_u64};

const CODE: &str = "session.governance_store.storage";

const REGISTRY_TABLE: &str = "_adk_governance_migrations";

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create governance state table",
    "\
CREATE TABLE IF NOT EXISTS adk_governance_state (\
    scope_key TEXT PRIMARY KEY, \
    scope TEXT NOT NULL, \
    frozen BOOLEAN NOT NULL, \
    reason TEXT, \
    issued_by TEXT, \
    issued_at_us BIGINT NOT NULL, \
    epoch BIGINT NOT NULL\
);",
)];

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const CREATE: &str = "\
INSERT INTO adk_governance_state \
    (scope_key, scope, frozen, reason, issued_by, issued_at_us, epoch) \
VALUES ($1, $2, $3, $4, $5, $6, 1) \
ON CONFLICT (scope_key) DO NOTHING";

const SWAP: &str = "\
UPDATE adk_governance_state \
SET frozen = $1, reason = $2, issued_by = $3, issued_at_us = $4, epoch = epoch + 1 \
WHERE scope_key = $5 AND epoch = $6";

const EPOCH: &str = "SELECT epoch FROM adk_governance_state WHERE scope_key = $1";

const GET: &str = "\
SELECT scope, frozen, reason, issued_by, issued_at_us, epoch \
FROM adk_governance_state WHERE scope_key = $1";

type StateRow = (String, bool, Option<String>, Option<String>, i64, i64);

fn decode(
    (scope, frozen, reason, issued_by, issued_at_us, epoch): StateRow,
) -> Result<GovernanceState> {
    Ok(GovernanceState {
        scope: from_json(CODE, "scope", &scope)?,
        frozen,
        reason,
        issued_by: issued_by.map(|json| from_json(CODE, "issued_by", &json)).transpose()?,
        issued_at: from_micros(CODE, "issued_at_us", issued_at_us)?,
        epoch: to_u64(CODE, "epoch", epoch)?,
    })
}

macro_rules! sql_governance_state_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty, $list:literal, $migrate:expr
    ) => {
        $(#[$doc])*
        #[derive(Debug, Clone)]
        pub struct $name {
            pool: $pool,
            clock: Arc<dyn Clock>,
        }

        impl $name {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.governance_store.storage` when the database cannot be
            /// reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    storage_error(CODE, format!("governance store connection failed: {e}"))
                })?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool, clock: Arc::new(SystemClock) }
            }

            /// Sets the clock that stamps `issued_at`.
            pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
                self.clock = clock;
                self
            }

            /// Creates or upgrades the governance state table.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration statement fails.
            pub async fn migrate(&self) -> Result<()> {
                let migrate = $migrate;
                migrate(&self.pool).await
            }
        }

        #[async_trait::async_trait]
        impl GovernanceStateStore for $name {
            async fn get(&self, scope: &GovernanceScope) -> Result<Option<GovernanceState>> {
                let row: Option<StateRow> = sqlx::query_as(GET)
                    .bind(scope.key())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("governance read failed: {e}")))?;
                row.map(decode).transpose()
            }

            async fn set(
                &self,
                scope: GovernanceScope,
                expected_epoch: u64,
                frozen: bool,
                reason: Option<String>,
                issued_by: Option<SubjectRef>,
            ) -> Result<GovernanceState> {
                let key = scope.key();
                let (issued_at, issued_at_us) = now(&*self.clock);
                let issued_by_json =
                    issued_by.as_ref().map(|subject| to_json(CODE, subject)).transpose()?;
                let failed =
                    |e: sqlx::Error| storage_error(CODE, format!("governance write failed: {e}"));
                let changed = if expected_epoch == 0 {
                    sqlx::query(CREATE)
                        .bind(&key)
                        .bind(to_json(CODE, &scope)?)
                        .bind(frozen)
                        .bind(reason.as_deref())
                        .bind(issued_by_json.as_deref())
                        .bind(issued_at_us)
                        .execute(&self.pool)
                        .await
                        .map_err(failed)?
                } else {
                    let Ok(expected) = i64::try_from(expected_epoch) else {
                        let current = self.get(&scope).await?.map_or(0, |state| state.epoch);
                        return Err(governance_conflict_error(&scope, expected_epoch, current));
                    };
                    sqlx::query(SWAP)
                        .bind(frozen)
                        .bind(reason.as_deref())
                        .bind(issued_by_json.as_deref())
                        .bind(issued_at_us)
                        .bind(&key)
                        .bind(expected)
                        .execute(&self.pool)
                        .await
                        .map_err(failed)?
                };
                if changed.rows_affected() == 0 {
                    let current: Option<i64> = sqlx::query_scalar(EPOCH)
                        .bind(&key)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(failed)?;
                    let current = current.map_or(Ok(0), |epoch| to_u64(CODE, "epoch", epoch))?;
                    return Err(governance_conflict_error(&scope, expected_epoch, current));
                }
                Ok(GovernanceState {
                    scope,
                    frozen,
                    reason,
                    issued_by,
                    issued_at,
                    epoch: expected_epoch + 1,
                })
            }

            async fn list(&self) -> Result<Vec<GovernanceState>> {
                let rows: Vec<StateRow> = sqlx::query_as($list)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| storage_error(CODE, format!("governance read failed: {e}")))?;
                rows.into_iter().map(decode).collect()
            }
        }
    };
}

#[cfg(feature = "sqlite")]
sql_governance_state_store! {
    /// SQLite-backed [`GovernanceStateStore`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::{GovernanceScope, GovernanceStateStore};
    /// use adk_session::SqliteGovernanceStateStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteGovernanceStateStore::new("sqlite://runtime.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// let current = store.get(&GovernanceScope::Org).await?.map_or(0, |state| state.epoch);
    /// store.set(GovernanceScope::Org, current, true, Some("incident 42".into()), None).await?;
    /// # Ok(())
    /// # }
    /// ```
    SqliteGovernanceStateStore, sqlx::SqlitePool,
    "SELECT scope, frozen, reason, issued_by, issued_at_us, epoch \
     FROM adk_governance_state ORDER BY scope_key",
    |pool: &sqlx::SqlitePool| {
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
sql_governance_state_store! {
    /// PostgreSQL-backed [`GovernanceStateStore`] for a kill switch shared by every
    /// replica. [`migrate`](Self::migrate) takes an advisory lock, so concurrent
    /// processes can call it at startup.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::{GovernanceScope, GovernanceStateStore};
    /// use adk_session::PostgresGovernanceStateStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresGovernanceStateStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// let frozen = store.get(&GovernanceScope::Org).await?.is_some_and(|state| state.frozen);
    /// # let _ = frozen;
    /// # Ok(())
    /// # }
    /// ```
    PostgresGovernanceStateStore, sqlx::PgPool,
    // Byte order, as `GovernanceScope::key` strings compare in Rust.
    "SELECT scope, frozen, reason, issued_by, issued_at_us, epoch \
     FROM adk_governance_state ORDER BY scope_key COLLATE \"C\"",
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
