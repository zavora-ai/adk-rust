//! PostgreSQL audit sink for enterprise audit logging.
//!
//! Stores audit events in a PostgreSQL table with full query, batch insert,
//! and retention (purge) support. Requires the `postgres-audit` feature.
//!
//! # Schema
//!
//! The sink auto-creates the `audit_events` table on first use via [`PostgresAuditSink::migrate()`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::audit_postgres::PostgresAuditSink;
//!
//! let sink = PostgresAuditSink::new("postgres://user:pass@localhost/adk").await?;
//! sink.migrate().await?;
//! sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)).await?;
//! ```

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::{debug, info};

use crate::AuthError;
use crate::audit::{AuditEvent, AuditEventType, AuditFilter, AuditOutcome, AuditSink};

/// PostgreSQL-backed audit sink.
///
/// Stores audit events in the `audit_events` table with indexed columns for
/// efficient querying by workspace, tenant, user, event type, and time range.
///
/// `Clone` is cheap — `PgPool` is `Arc`-based internally, so cloning just
/// increments a reference count. No need to wrap in `Arc<PostgresAuditSink>`.
#[derive(Clone)]
pub struct PostgresAuditSink {
    pool: PgPool,
}

impl PostgresAuditSink {
    /// Connect to PostgreSQL and create a new audit sink.
    ///
    /// Call [`migrate()`](Self::migrate) after construction to ensure the table exists.
    pub async fn new(database_url: &str) -> Result<Self, AuthError> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| AuthError::AuditError(format!("postgres connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Create a new audit sink from an existing connection pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run database migrations to create the audit_events table.
    ///
    /// Safe to call multiple times — uses `CREATE TABLE IF NOT EXISTS`.
    /// Executes each statement individually since sqlx doesn't support
    /// multi-statement queries.
    pub async fn migrate(&self) -> Result<(), AuthError> {
        for &statement in MIGRATION_STATEMENTS {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(|e| AuthError::AuditError(format!("migration failed: {e}")))?;
        }
        info!("audit_events table ready");
        Ok(())
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

const MIGRATION_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS audit_events (
        id BIGSERIAL PRIMARY KEY,
        timestamp TIMESTAMPTZ NOT NULL,
        user_id TEXT NOT NULL,
        session_id TEXT,
        event_type TEXT NOT NULL,
        resource TEXT NOT NULL,
        outcome TEXT NOT NULL,
        metadata JSONB,
        workspace_id TEXT,
        tenant_id TEXT,
        request_id TEXT,
        ip_address TEXT,
        resource_id TEXT,
        action TEXT,
        prev_hash TEXT
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_audit_workspace ON audit_events (workspace_id, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_audit_tenant ON audit_events (tenant_id, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_events (user_id, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_audit_event_type ON audit_events (event_type, timestamp DESC)",
    "CREATE INDEX IF NOT EXISTS idx_audit_resource_id ON audit_events (resource_id) WHERE resource_id IS NOT NULL",
];

#[async_trait::async_trait]
impl AuditSink for PostgresAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        let event_type_str = serde_json::to_value(&event.event_type)
            .map_err(|e| AuthError::AuditError(format!("serialize event_type: {e}")))?
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        let outcome_str = serde_json::to_value(&event.outcome)
            .map_err(|e| AuthError::AuditError(format!("serialize outcome: {e}")))?
            .as_str()
            .unwrap_or("unknown")
            .to_string();

        sqlx::query(
            r#"INSERT INTO audit_events
               (timestamp, user_id, session_id, event_type, resource, outcome,
                metadata, workspace_id, tenant_id, request_id, ip_address,
                resource_id, action, prev_hash)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
        )
        .bind(event.timestamp)
        .bind(&event.user)
        .bind(&event.session_id)
        .bind(&event_type_str)
        .bind(&event.resource)
        .bind(&outcome_str)
        .bind(&event.metadata)
        .bind(&event.workspace_id)
        .bind(&event.tenant_id)
        .bind(&event.request_id)
        .bind(&event.ip_address)
        .bind(&event.resource_id)
        .bind(&event.action)
        .bind(&event.prev_hash)
        .execute(&self.pool)
        .await
        .map_err(|e| AuthError::AuditError(format!("insert failed: {e}")))?;

        debug!("audit event logged to postgres");
        Ok(())
    }

    async fn log_batch(&self, events: Vec<AuditEvent>) -> Result<(), AuthError> {
        if events.is_empty() {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AuthError::AuditError(format!("transaction begin failed: {e}")))?;

        for event in &events {
            let event_type_str = serde_json::to_value(&event.event_type)
                .map_err(|e| AuthError::AuditError(format!("serialize event_type: {e}")))?
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let outcome_str = serde_json::to_value(&event.outcome)
                .map_err(|e| AuthError::AuditError(format!("serialize outcome: {e}")))?
                .as_str()
                .unwrap_or("unknown")
                .to_string();

            sqlx::query(
                r#"INSERT INTO audit_events
                   (timestamp, user_id, session_id, event_type, resource, outcome,
                    metadata, workspace_id, tenant_id, request_id, ip_address,
                    resource_id, action, prev_hash)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)"#,
            )
            .bind(event.timestamp)
            .bind(&event.user)
            .bind(&event.session_id)
            .bind(&event_type_str)
            .bind(&event.resource)
            .bind(&outcome_str)
            .bind(&event.metadata)
            .bind(&event.workspace_id)
            .bind(&event.tenant_id)
            .bind(&event.request_id)
            .bind(&event.ip_address)
            .bind(&event.resource_id)
            .bind(&event.action)
            .bind(&event.prev_hash)
            .execute(&mut *tx)
            .await
            .map_err(|e| AuthError::AuditError(format!("batch insert failed: {e}")))?;
        }

        tx.commit()
            .await
            .map_err(|e| AuthError::AuditError(format!("transaction commit failed: {e}")))?;

        debug!(count = events.len(), "audit batch logged to postgres");
        Ok(())
    }

    async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEvent>, AuthError> {
        let mut conditions = Vec::new();
        let mut bind_idx = 1u32;

        // Build dynamic WHERE clause
        if filter.user.is_some() {
            conditions.push(format!("user_id = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.workspace_id.is_some() {
            conditions.push(format!("workspace_id = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.tenant_id.is_some() {
            conditions.push(format!("tenant_id = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.event_type.is_some() {
            conditions.push(format!("event_type = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.outcome.is_some() {
            conditions.push(format!("outcome = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.resource.is_some() {
            conditions.push(format!("resource LIKE ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.resource_id.is_some() {
            conditions.push(format!("resource_id = ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.after.is_some() {
            conditions.push(format!("timestamp > ${bind_idx}"));
            bind_idx += 1;
        }
        if filter.before.is_some() {
            conditions.push(format!("timestamp < ${bind_idx}"));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let limit = filter.limit.unwrap_or(1000);
        let offset = filter.offset.unwrap_or(0);

        let sql = format!(
            "SELECT timestamp, user_id, session_id, event_type, resource, outcome, \
             metadata, workspace_id, tenant_id, request_id, ip_address, resource_id, \
             action, prev_hash \
             FROM audit_events {where_clause} \
             ORDER BY timestamp DESC \
             LIMIT {limit} OFFSET {offset}"
        );

        // Only fixed column predicates with numbered placeholders and the integer
        // limit and offset are formatted in; every filter value is bound.
        let mut query = sqlx::query_as::<_, AuditRow>(sqlx::AssertSqlSafe(sql));

        if let Some(ref user) = filter.user {
            query = query.bind(user);
        }
        if let Some(ref ws) = filter.workspace_id {
            query = query.bind(ws);
        }
        if let Some(ref tid) = filter.tenant_id {
            query = query.bind(tid);
        }
        if let Some(ref et) = filter.event_type {
            let et_str = serde_json::to_value(et)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            query = query.bind(et_str);
        }
        if let Some(ref oc) = filter.outcome {
            let oc_str = serde_json::to_value(oc)
                .unwrap_or_default()
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            query = query.bind(oc_str);
        }
        if let Some(ref res) = filter.resource {
            query = query.bind(format!("%{res}%"));
        }
        if let Some(ref rid) = filter.resource_id {
            query = query.bind(rid);
        }
        if let Some(after) = filter.after {
            query = query.bind(after);
        }
        if let Some(before) = filter.before {
            query = query.bind(before);
        }

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|e| AuthError::AuditError(format!("query failed: {e}")))?;

        let events = rows.into_iter().map(|row| row.into_event()).collect();
        Ok(events)
    }

    async fn purge_before(&self, cutoff: DateTime<Utc>) -> Result<u64, AuthError> {
        let result = sqlx::query("DELETE FROM audit_events WHERE timestamp < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(|e| AuthError::AuditError(format!("purge failed: {e}")))?;

        let count = result.rows_affected();
        info!(purged = count, cutoff = %cutoff, "audit events purged");
        Ok(count)
    }
}

/// Internal row type for sqlx deserialization.
#[derive(sqlx::FromRow)]
struct AuditRow {
    timestamp: DateTime<Utc>,
    user_id: String,
    session_id: Option<String>,
    event_type: String,
    resource: String,
    outcome: String,
    metadata: Option<serde_json::Value>,
    workspace_id: Option<String>,
    tenant_id: Option<String>,
    request_id: Option<String>,
    ip_address: Option<String>,
    resource_id: Option<String>,
    action: Option<String>,
    prev_hash: Option<String>,
}

impl AuditRow {
    fn into_event(self) -> AuditEvent {
        let event_type = serde_json::from_value::<AuditEventType>(serde_json::Value::String(
            self.event_type.clone(),
        ))
        .unwrap_or(AuditEventType::Custom(self.event_type));

        let outcome =
            serde_json::from_value::<AuditOutcome>(serde_json::Value::String(self.outcome.clone()))
                .unwrap_or(AuditOutcome::Error);

        AuditEvent {
            timestamp: self.timestamp,
            user: self.user_id,
            session_id: self.session_id,
            event_type,
            resource: self.resource,
            outcome,
            metadata: self.metadata,
            workspace_id: self.workspace_id,
            tenant_id: self.tenant_id,
            request_id: self.request_id,
            ip_address: self.ip_address,
            resource_id: self.resource_id,
            action: self.action,
            prev_hash: self.prev_hash,
        }
    }
}
