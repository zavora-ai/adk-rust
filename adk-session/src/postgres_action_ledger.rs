use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, Result, ToolEffect,
    action_ledger::{already_completed_error, duplicate_key_error, missing_key_error},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, postgres::PgPool};

/// PostgreSQL-backed [`ActionLedger`], so a non-idempotent tool call is never repeated
/// across a process restart or by another process sharing the database.
///
/// Each record stores the idempotency key, tool name, argument digest, effect, start
/// time, and outcome — never the arguments or result themselves. `begin` is an
/// `INSERT ... ON CONFLICT DO NOTHING` on the key and `complete` an `UPDATE` guarded by
/// `outcome IS NULL`, so concurrent callers see exactly one winner. Timestamps are
/// stored as `TIMESTAMPTZ`, with microsecond precision.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::PostgresActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = PostgresActionLedger::new("postgres://localhost/app").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresActionLedger {
    pool: PgPool,
}

impl PostgresActionLedger {
    /// The registry table used to track applied migration versions.
    const REGISTRY_TABLE: &'static str = "_adk_action_ledger_migrations";

    /// Compiled-in migration steps, as `(version, description, sql)`.
    const MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[(
        1,
        "create action ledger table",
        "\
CREATE TABLE IF NOT EXISTS adk_action_ledger (\
    idempotency_key TEXT PRIMARY KEY, \
    tool_name TEXT NOT NULL, \
    args_digest TEXT NOT NULL, \
    effect TEXT NOT NULL, \
    started_at TIMESTAMPTZ NOT NULL, \
    outcome JSONB, \
    completed_at TIMESTAMPTZ\
);",
    )];

    /// Connects to PostgreSQL and creates a connection pool.
    ///
    /// Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be reached.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| ledger_error(format!("action ledger database connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Creates a ledger from an existing connection pool, such as the one a
    /// [`PostgresSessionService`](crate::PostgresSessionService) uses.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates the ledger table if it does not exist, under an advisory lock so
    /// concurrent processes can call it at startup.
    ///
    /// # Errors
    ///
    /// Returns an error when a migration step fails.
    pub async fn migrate(&self) -> Result<()> {
        crate::migration::run_pg_migrations_locked(
            &self.pool,
            Self::REGISTRY_TABLE,
            Self::MIGRATIONS,
        )
        .await
    }
}

fn ledger_error(message: String) -> AdkError {
    AdkError::new(
        adk_core::ErrorComponent::Session,
        adk_core::ErrorCategory::Unavailable,
        "session.action_ledger.storage",
        message,
    )
}

fn corrupt(column: &str, error: impl std::fmt::Display) -> AdkError {
    ledger_error(format!("action ledger column '{column}' is corrupt: {error}"))
}

#[async_trait]
impl ActionLedger for PostgresActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let effect = serde_json::to_string(&record.effect)
            .map_err(|e| ledger_error(format!("action ledger serialize failed: {e}")))?;
        let outcome = record
            .outcome
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| ledger_error(format!("action ledger serialize failed: {e}")))?;
        let inserted = sqlx::query(
            "INSERT INTO adk_action_ledger \
             (idempotency_key, tool_name, args_digest, effect, started_at, outcome, completed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(&record.key)
        .bind(&record.tool_name)
        .bind(&record.args_digest)
        .bind(effect)
        .bind(record.started_at)
        .bind(outcome)
        .bind(record.completed_at)
        .execute(&self.pool)
        .await
        .map_err(|e| ledger_error(format!("action ledger begin failed: {e}")))?;
        if inserted.rows_affected() == 0 {
            return Err(duplicate_key_error(&record.key));
        }
        Ok(())
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let outcome = serde_json::to_value(&outcome)
            .map_err(|e| ledger_error(format!("action ledger serialize failed: {e}")))?;
        let updated = sqlx::query(
            "UPDATE adk_action_ledger SET outcome = $1, completed_at = $2 \
             WHERE idempotency_key = $3 AND outcome IS NULL",
        )
        .bind(outcome)
        .bind(Utc::now())
        .bind(key)
        .execute(&self.pool)
        .await
        .map_err(|e| ledger_error(format!("action ledger complete failed: {e}")))?;
        if updated.rows_affected() == 0 {
            return Err(match self.get(key).await? {
                Some(_) => already_completed_error(key),
                None => missing_key_error(key),
            });
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<ActionRecord>> {
        let row = sqlx::query(
            "SELECT idempotency_key, tool_name, args_digest, effect, started_at, outcome, \
             completed_at FROM adk_action_ledger WHERE idempotency_key = $1",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ledger_error(format!("action ledger read failed: {e}")))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let effect: String = row.try_get("effect").map_err(|e| corrupt("effect", e))?;
        let effect: ToolEffect = serde_json::from_str(&effect).map_err(|e| corrupt("effect", e))?;
        let outcome: Option<serde_json::Value> =
            row.try_get("outcome").map_err(|e| corrupt("outcome", e))?;
        Ok(Some(ActionRecord {
            key: row.try_get("idempotency_key").map_err(|e| corrupt("idempotency_key", e))?,
            tool_name: row.try_get("tool_name").map_err(|e| corrupt("tool_name", e))?,
            args_digest: row.try_get("args_digest").map_err(|e| corrupt("args_digest", e))?,
            effect,
            started_at: row
                .try_get::<DateTime<Utc>, _>("started_at")
                .map_err(|e| corrupt("started_at", e))?,
            outcome: outcome
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| corrupt("outcome", e))?,
            completed_at: row
                .try_get::<Option<DateTime<Utc>>, _>("completed_at")
                .map_err(|e| corrupt("completed_at", e))?,
        }))
    }
}
