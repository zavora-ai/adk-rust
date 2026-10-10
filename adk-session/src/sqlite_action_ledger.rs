use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, AdkError, Result, ToolEffect,
    action_ledger::{already_completed_error, duplicate_key_error, missing_key_error},
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Row, sqlite::SqlitePool};

/// SQLite-backed [`ActionLedger`], so a non-idempotent tool call is never repeated
/// across a process restart.
///
/// Each record stores the idempotency key, tool name, argument digest, effect,
/// start time, and outcome — never the arguments or result themselves.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::RunConfig;
/// use adk_session::SqliteActionLedger;
/// use std::sync::Arc;
///
/// # async fn example() -> adk_core::Result<()> {
/// let ledger = SqliteActionLedger::new("sqlite:actions.db?mode=rwc").await?;
/// ledger.migrate().await?;
/// let config = RunConfig::builder().action_ledger(Arc::new(ledger)).build();
/// # let _ = config;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteActionLedger {
    pool: SqlitePool,
}

impl SqliteActionLedger {
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
    started_at TEXT NOT NULL, \
    outcome TEXT, \
    completed_at TEXT\
);",
    )];

    /// Connects to SQLite and creates a connection pool.
    ///
    /// Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url)
            .await
            .map_err(|e| ledger_error(format!("action ledger database connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Creates a ledger from an existing connection pool, such as the one a
    /// [`SqliteSessionService`](crate::SqliteSessionService) uses.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates the ledger table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error when a migration step fails.
    pub async fn migrate(&self) -> Result<()> {
        let pool = &self.pool;
        crate::migration::sqlite_runner::run_sql_migrations(
            pool,
            Self::REGISTRY_TABLE,
            Self::MIGRATIONS,
            || async {
                let row = sqlx::query(
                    "SELECT COUNT(*) AS cnt FROM sqlite_master \
                     WHERE type='table' AND name='adk_action_ledger'",
                )
                .fetch_one(pool)
                .await
                .map_err(|e| ledger_error(format!("baseline detection failed: {e}")))?;
                let count: i64 = row.try_get("cnt").unwrap_or(0);
                Ok(count > 0)
            },
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

fn to_json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| ledger_error(format!("action ledger serialize failed: {e}")))
}

fn from_json<T: serde::de::DeserializeOwned>(json: &str, column: &str) -> Result<T> {
    serde_json::from_str(json)
        .map_err(|e| ledger_error(format!("action ledger column '{column}' is corrupt: {e}")))
}

fn parse_time(text: &str, column: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|e| ledger_error(format!("action ledger column '{column}' is corrupt: {e}")))
}

#[async_trait]
impl ActionLedger for SqliteActionLedger {
    async fn begin(&self, record: &ActionRecord) -> Result<()> {
        let inserted = sqlx::query(
            "INSERT INTO adk_action_ledger \
             (idempotency_key, tool_name, args_digest, effect, started_at, outcome, completed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(idempotency_key) DO NOTHING",
        )
        .bind(&record.key)
        .bind(&record.tool_name)
        .bind(&record.args_digest)
        .bind(to_json(&record.effect)?)
        .bind(record.started_at.to_rfc3339())
        .bind(record.outcome.as_ref().map(to_json).transpose()?)
        .bind(record.completed_at.map(|time| time.to_rfc3339()))
        .execute(&self.pool)
        .await
        .map_err(|e| ledger_error(format!("action ledger begin failed: {e}")))?;
        if inserted.rows_affected() == 0 {
            return Err(duplicate_key_error(&record.key));
        }
        Ok(())
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> Result<()> {
        let updated = sqlx::query(
            "UPDATE adk_action_ledger SET outcome = ?, completed_at = ? \
             WHERE idempotency_key = ? AND outcome IS NULL",
        )
        .bind(to_json(&outcome)?)
        .bind(Utc::now().to_rfc3339())
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
             completed_at FROM adk_action_ledger WHERE idempotency_key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ledger_error(format!("action ledger read failed: {e}")))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let column = |name: &str| -> Result<Option<String>> {
            row.try_get(name)
                .map_err(|e| ledger_error(format!("action ledger column '{name}' unreadable: {e}")))
        };
        let required = |name: &str| -> Result<String> {
            column(name)?
                .ok_or_else(|| ledger_error(format!("action ledger column '{name}' is empty")))
        };
        let effect: ToolEffect = from_json(&required("effect")?, "effect")?;
        Ok(Some(ActionRecord {
            key: required("idempotency_key")?,
            tool_name: required("tool_name")?,
            args_digest: required("args_digest")?,
            effect,
            started_at: parse_time(&required("started_at")?, "started_at")?,
            outcome: column("outcome")?.map(|json| from_json(&json, "outcome")).transpose()?,
            completed_at: column("completed_at")?
                .map(|text| parse_time(&text, "completed_at"))
                .transpose()?,
        }))
    }
}
