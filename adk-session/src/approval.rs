//! SQL-backed [`ApprovalStore`](adk_core::ApprovalStore) implementations.
//!
//! Calls held for confirmation and the decisions recorded for them survive restarts
//! and are shared by every process that points at the same database.
//!
//! | Table | Key | Holds |
//! |-------|-----|-------|
//! | `adk_approval_pending` | app, user, session, fingerprint | The first request and when it was held |
//! | `adk_approval_decisions` | app, user, session, fingerprint | The [`ToolApproval`](adk_core::ToolApproval) as JSON, with any edited arguments and message |
//! | `adk_approval_requests` | id | An [`ApprovalRequest`](adk_core::ApprovalRequest) as JSON, with its application, state, openness, creation time, and version as indexed columns |
//! | `adk_approval_delegations` | id | An [`ApprovalDelegation`](adk_core::ApprovalDelegation) as JSON, with its delegate as indexed columns |
//!
//! | Backend | Feature | How a decision is consumed once | How a request changes once per version |
//! |---------|---------|---------------------------------|----------------------------------------|
//! | [`SqliteApprovalStore`](crate::SqliteApprovalStore) | `sqlite` | `DELETE ... RETURNING` under SQLite's single writer | `UPDATE ... WHERE version = $expected` under the single writer |
//! | [`PostgresApprovalStore`](crate::PostgresApprovalStore) | `postgres` | `DELETE ... RETURNING`; a concurrent delete of the same row waits and then finds it gone | `UPDATE ... WHERE version = $expected`; a concurrent update waits for the row lock and re-checks the version |

use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ApprovalDelegation, ApprovalRequest, ApprovalRequestQuery, ApprovalScope,
    ApprovalStore, PendingApproval, Result, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationRequest,
};
use chrono::{DateTime, Utc};

const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
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
    ),
    (
        2,
        "create approval request and delegation tables",
        "\
CREATE TABLE IF NOT EXISTS adk_approval_requests (\
    id TEXT NOT NULL PRIMARY KEY, \
    app_name TEXT NOT NULL, \
    state TEXT NOT NULL, \
    open_flag BIGINT NOT NULL, \
    created_at_us BIGINT NOT NULL, \
    version BIGINT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_approval_requests_open \
    ON adk_approval_requests (open_flag, created_at_us, id);\
CREATE INDEX IF NOT EXISTS adk_approval_requests_app \
    ON adk_approval_requests (app_name, created_at_us, id);\
CREATE TABLE IF NOT EXISTS adk_approval_delegations (\
    id TEXT NOT NULL PRIMARY KEY, \
    to_kind TEXT NOT NULL, \
    to_id TEXT NOT NULL, \
    record TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_approval_delegations_to \
    ON adk_approval_delegations (to_kind, to_id, id);",
    ),
];

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

const CREATE_REQUEST: &str = "\
INSERT INTO adk_approval_requests \
    (id, app_name, state, open_flag, created_at_us, version, record) \
VALUES ($1, $2, $3, $4, $5, $6, $7) \
ON CONFLICT (id) DO NOTHING";

const READ_REQUEST: &str = "SELECT record FROM adk_approval_requests WHERE id = $1";

const REPLACE_REQUEST: &str = "\
UPDATE adk_approval_requests \
SET app_name = $1, state = $2, open_flag = $3, created_at_us = $4, version = $5, record = $6 \
WHERE id = $7 AND version = $8";

const PUT_DELEGATION: &str = "\
INSERT INTO adk_approval_delegations (id, to_kind, to_id, record) VALUES ($1, $2, $3, $4) \
ON CONFLICT (id) DO UPDATE SET \
    to_kind = excluded.to_kind, to_id = excluded.to_id, record = excluded.record";

const LIST_DELEGATIONS: &str = "\
SELECT record FROM adk_approval_delegations WHERE to_kind = $1 AND to_id = $2 ORDER BY id";

const DELETE_DELEGATION: &str = "DELETE FROM adk_approval_delegations WHERE id = $1";

/// The indexed columns of a stored request.
struct RequestColumns {
    state: &'static str,
    open_flag: i64,
    created_at_us: i64,
    version: i64,
    record: String,
}

fn request_columns(request: &ApprovalRequest) -> Result<RequestColumns> {
    Ok(RequestColumns {
        state: request.state.as_str(),
        open_flag: i64::from(request.is_open()),
        created_at_us: request.created_at.timestamp_micros(),
        version: stored_version(request.version)?,
        record: to_json(request)?,
    })
}

fn stored_version(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| store_error(format!("approval request version {version} is out of range")))
}

fn subject_kind(subject: &SubjectRef) -> Result<String> {
    match serde_json::to_value(subject.kind) {
        Ok(serde_json::Value::String(kind)) => Ok(kind),
        _ => Err(store_error("approval store could not encode a subject kind".to_string())),
    }
}

/// Builds the `list_requests` statement with its parameters numbered in order of first
/// appearance, so SQLite and PostgreSQL bind them alike.
fn list_statement(query: &ApprovalRequestQuery) -> (String, Vec<SqlParam>) {
    let mut sql = String::from("SELECT record FROM adk_approval_requests WHERE 1 = 1");
    let mut params = Vec::new();
    fn next(params: &mut Vec<SqlParam>, param: SqlParam) -> String {
        params.push(param);
        format!("${}", params.len())
    }
    if let Some(app) = &query.app_name {
        let p = next(&mut params, SqlParam::Text(app.clone()));
        sql.push_str(&format!(" AND app_name = {p}"));
    }
    if !query.states.is_empty() {
        let placeholders: Vec<String> = query
            .states
            .iter()
            .map(|state| next(&mut params, SqlParam::Text(state.as_str().to_string())))
            .collect();
        sql.push_str(&format!(" AND state IN ({})", placeholders.join(", ")));
    }
    if query.open_only {
        sql.push_str(" AND open_flag = 1");
    }
    if let Some(after) = &query.after {
        let at = next(&mut params, SqlParam::Int(after.created_at.timestamp_micros()));
        let id = next(&mut params, SqlParam::Text(after.id.clone()));
        sql.push_str(&format!(
            " AND (created_at_us > {at} OR (created_at_us = {at} AND id > {id}))"
        ));
    }
    let limit = next(&mut params, SqlParam::Int(i64::try_from(query.limit).unwrap_or(i64::MAX)));
    sql.push_str(&format!(" ORDER BY created_at_us, id LIMIT {limit}"));
    (sql, params)
}

enum SqlParam {
    Text(String),
    Int(i64),
}

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

/// A stored approval when it is still live.
fn live_approval(approval: &str) -> Result<Option<ToolApproval>> {
    let approval: ToolApproval = from_json(approval, "approval")?;
    Ok(approval.is_live(Utc::now()).then_some(approval))
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
                Ok(self.take_approval(scope, fingerprint).await?.map(|approval| approval.decision))
            }

            async fn take_approval(
                &self,
                scope: &ApprovalScope,
                fingerprint: &str,
            ) -> Result<Option<ToolApproval>> {
                let approval: Option<String> = sqlx::query_scalar(TAKE_DECISION)
                    .bind(&scope.app_name)
                    .bind(&scope.user_id)
                    .bind(&scope.session_id)
                    .bind(fingerprint)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                approval.map_or(Ok(None), |approval| live_approval(&approval))
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
                    Some(approval) => live_approval(&approval)?.is_some(),
                    None => false,
                })
            }

            async fn create_request(&self, request: &ApprovalRequest) -> Result<ApprovalRequest> {
                let columns = request_columns(request)?;
                sqlx::query(CREATE_REQUEST)
                    .bind(&request.id)
                    .bind(&request.scope.app_name)
                    .bind(columns.state)
                    .bind(columns.open_flag)
                    .bind(columns.created_at_us)
                    .bind(columns.version)
                    .bind(&columns.record)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                self.get_request(&request.id).await?.ok_or_else(|| {
                    store_error(format!("approval request '{}' vanished after it was created", request.id))
                })
            }

            async fn get_request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
                let record: Option<String> = sqlx::query_scalar(READ_REQUEST)
                    .bind(id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store read failed: {e}")))?;
                record.map(|record| from_json(&record, "record")).transpose()
            }

            async fn replace_request(
                &self,
                request: &ApprovalRequest,
                expected_version: u64,
            ) -> Result<bool> {
                let columns = request_columns(request)?;
                let updated = sqlx::query(REPLACE_REQUEST)
                    .bind(&request.scope.app_name)
                    .bind(columns.state)
                    .bind(columns.open_flag)
                    .bind(columns.created_at_us)
                    .bind(columns.version)
                    .bind(&columns.record)
                    .bind(&request.id)
                    .bind(stored_version(expected_version)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                Ok(updated.rows_affected() == 1)
            }

            async fn list_requests(
                &self,
                query: &ApprovalRequestQuery,
            ) -> Result<Vec<ApprovalRequest>> {
                let (sql, params) = list_statement(query);
                let mut statement = sqlx::query_scalar::<_, String>(&sql);
                for param in params {
                    statement = match param {
                        SqlParam::Text(text) => statement.bind(text),
                        SqlParam::Int(value) => statement.bind(value),
                    };
                }
                let records = statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store read failed: {e}")))?;
                records.iter().map(|record| from_json(record, "record")).collect()
            }

            async fn put_delegation(&self, delegation: &ApprovalDelegation) -> Result<()> {
                sqlx::query(PUT_DELEGATION)
                    .bind(&delegation.id)
                    .bind(subject_kind(&delegation.to)?)
                    .bind(&delegation.to.id)
                    .bind(to_json(delegation)?)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                Ok(())
            }

            async fn delegations_to(&self, to: &SubjectRef) -> Result<Vec<ApprovalDelegation>> {
                let records: Vec<String> = sqlx::query_scalar(LIST_DELEGATIONS)
                    .bind(subject_kind(to)?)
                    .bind(&to.id)
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store read failed: {e}")))?;
                records.iter().map(|record| from_json(record, "record")).collect()
            }

            async fn delete_delegation(&self, id: &str) -> Result<bool> {
                let deleted = sqlx::query(DELETE_DELEGATION)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| store_error(format!("approval store write failed: {e}")))?;
                Ok(deleted.rows_affected() > 0)
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
