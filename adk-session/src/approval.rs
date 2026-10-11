//! SQL-backed [`ApprovalStore`](adk_core::ApprovalStore) implementations.
//!
//! Calls held for confirmation and the decisions recorded for them survive restarts
//! and are shared by every process that points at the same database.
//!
//! | Table | Key | Holds |
//! |-------|-----|-------|
//! | `adk_approval_pending` | app, user, session, fingerprint | The first request and when it was held |
//! | `adk_approval_decisions` | app, user, session, fingerprint | The [`ToolApproval`](adk_core::ToolApproval) as JSON |
//!
//! | Backend | Feature | How a decision is consumed once |
//! |---------|---------|---------------------------------|
//! | [`SqliteApprovalStore`](crate::SqliteApprovalStore) | `sqlite` | `DELETE ... RETURNING` under SQLite's single writer |
//! | [`PostgresApprovalStore`](crate::PostgresApprovalStore) | `postgres` | `DELETE ... RETURNING`; a concurrent delete of the same row waits and then finds it gone |

use adk_core::{
    AdkError, ApprovalScope, ApprovalStore, PendingApproval, Result, ToolApproval,
    ToolConfirmationDecision, ToolConfirmationRequest,
};
use chrono::{DateTime, Utc};

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create approval tables",
    "\
CREATE TABLE IF NOT EXISTS adk_approval_pending (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    request TEXT NOT NULL, \
    requested_at_us BIGINT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, fingerprint)\
);\
CREATE TABLE IF NOT EXISTS adk_approval_decisions (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    fingerprint TEXT NOT NULL, \
    approval TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id, session_id, fingerprint)\
);",
)];

const REGISTRY_TABLE: &str = "_adk_approval_migrations";

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const RECORD_PENDING: &str = "\
INSERT INTO adk_approval_pending \
    (app_name, user_id, session_id, fingerprint, request, requested_at_us) \
VALUES ($1, $2, $3, $4, $5, $6) \
ON CONFLICT (app_name, user_id, session_id, fingerprint) DO NOTHING";

const LIST_PENDING: &str = "\
SELECT fingerprint, request, requested_at_us FROM adk_approval_pending \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 \
ORDER BY requested_at_us, fingerprint";

const CLEAR_PENDING: &str = "\
DELETE FROM adk_approval_pending \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 AND fingerprint = $4";

const RECORD_DECISION: &str = "\
INSERT INTO adk_approval_decisions (app_name, user_id, session_id, fingerprint, approval) \
VALUES ($1, $2, $3, $4, $5) \
ON CONFLICT (app_name, user_id, session_id, fingerprint) DO UPDATE SET approval = excluded.approval";

const TAKE_DECISION: &str = "\
DELETE FROM adk_approval_decisions \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 AND fingerprint = $4 \
RETURNING approval";

const READ_DECISION: &str = "\
SELECT approval FROM adk_approval_decisions \
WHERE app_name = $1 AND user_id = $2 AND session_id = $3 AND fingerprint = $4";

fn store_error(message: String) -> AdkError {
    AdkError::new(
        adk_core::ErrorComponent::Session,
        adk_core::ErrorCategory::Unavailable,
        "session.approval_store.storage",
        message,
    )
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| store_error(format!("approval store serialize failed: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(json: &str, column: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|e| store_error(format!("approval store column '{column}' is corrupt: {e}")))
}

fn from_micros(micros: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_micros(micros).ok_or_else(|| {
        store_error(format!("approval store column 'requested_at_us' is out of range: {micros}"))
    })
}

/// The decision in a stored approval when it is still live.
fn live_decision(approval: &str) -> Result<Option<ToolConfirmationDecision>> {
    let approval: ToolApproval = from_json(approval, "approval")?;
    Ok(approval.is_live(Utc::now()).then_some(approval.decision))
}

macro_rules! sql_approval_store {
    (
        $(#[$doc:meta])*
        $name:ident, $pool:ty, $migrate:expr
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
            /// Returns `session.approval_store.storage` when the database cannot be
            /// reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    store_error(format!("approval store database connection failed: {e}"))
                })?;
                Ok(Self { pool })
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool }
            }

            /// Creates or upgrades the approval tables.
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
        impl ApprovalStore for $name {
            async fn record_pending(
                &self,
                scope: &ApprovalScope,
                request: &ToolConfirmationRequest,
            ) -> Result<()> {
                sqlx::query(RECORD_PENDING)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(request.fingerprint())
                    .bind(to_json(request)?)
                    .bind(Utc::now().timestamp_micros())
                    .execute(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                Ok(())
            }

            async fn pending(&self, scope: &ApprovalScope) -> Result<Vec<PendingApproval>> {
                use sqlx::Row;
                let rows = sqlx::query(LIST_PENDING)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store read failed: {e}")))?;
                rows.iter()
                    .map(|row| {
                        let column = |name: &str| {
                            store_error(format!("approval store column '{name}' unreadable"))
                        };
                        let request: String =
                            row.try_get("request").map_err(|_| column("request"))?;
                        Ok(PendingApproval {
                            fingerprint: row
                                .try_get("fingerprint")
                                .map_err(|_| column("fingerprint"))?,
                            request: from_json(&request, "request")?,
                            requested_at: from_micros(
                                row.try_get("requested_at_us")
                                    .map_err(|_| column("requested_at_us"))?,
                            )?,
                        })
                    })
                    .collect()
            }

            async fn decide(
                &self,
                scope: &ApprovalScope,
                fingerprint: &str,
                approval: ToolApproval,
            ) -> Result<()> {
                let failed = |e: sqlx::Error| store_error(format!("approval store write failed: {e}"));
                let mut tx = self.pool.begin().await.map_err(failed)?;
                sqlx::query(CLEAR_PENDING)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(fingerprint)
                    .execute(&mut *tx)
                    .await
                    .map_err(failed)?;
                sqlx::query(RECORD_DECISION)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(fingerprint)
                    .bind(to_json(&approval)?)
                    .execute(&mut *tx)
                    .await
                    .map_err(failed)?;
                tx.commit().await.map_err(failed)
            }

            async fn take_decision(
                &self,
                scope: &ApprovalScope,
                fingerprint: &str,
            ) -> Result<Option<ToolConfirmationDecision>> {
                let approval: Option<String> = sqlx::query_scalar(TAKE_DECISION)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(fingerprint)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                approval.map_or(Ok(None), |approval| live_decision(&approval))
            }

            async fn has_decision(&self, scope: &ApprovalScope, fingerprint: &str) -> Result<bool> {
                let approval: Option<String> = sqlx::query_scalar(READ_DECISION)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(fingerprint)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store read failed: {e}")))?;
                Ok(match approval {
                    Some(approval) => live_decision(&approval)?.is_some(),
                    None => false,
                })
            }
        }
    };
}

#[cfg(feature = "sqlite")]
sql_approval_store! {
    /// SQLite-backed [`ApprovalStore`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::RunConfig;
    /// use adk_session::SqliteApprovalStore;
    /// use std::sync::Arc;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = SqliteApprovalStore::new("sqlite://approvals.db?mode=rwc").await?;
    /// store.migrate().await?;
    /// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
    /// # let _ = config;
    /// # Ok(())
    /// # }
    /// ```
    SqliteApprovalStore, sqlx::SqlitePool,
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
sql_approval_store! {
    /// PostgreSQL-backed [`ApprovalStore`]. [`migrate`](Self::migrate) takes an advisory
    /// lock, so concurrent processes can call it at startup.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::RunConfig;
    /// use adk_session::PostgresApprovalStore;
    /// use std::sync::Arc;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let store = PostgresApprovalStore::new("postgres://localhost/app").await?;
    /// store.migrate().await?;
    /// let config = RunConfig::builder().approval_store(Arc::new(store)).build();
    /// # let _ = config;
    /// # Ok(())
    /// # }
    /// ```
    PostgresApprovalStore, sqlx::PgPool,
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
