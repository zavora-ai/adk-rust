use crate::{
    CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP, ListRequest, Session,
    SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use tracing::instrument;
use uuid::Uuid;

/// PostgreSQL-backed session service.
///
/// Uses `sqlx::PgPool` for connection pooling and supports the full
/// three-tier state model (app, user, session) with `JSONB` columns
/// and `TIMESTAMPTZ` timestamps.
///
/// # Example
///
/// ```rust,ignore
/// let service = PostgresSessionService::new("postgres://user:pass@localhost/mydb").await?;
/// service.migrate().await?;
/// ```
pub struct PostgresSessionService {
    pool: PgPool,
}

impl PostgresSessionService {
    /// Connect to PostgreSQL and create a connection pool.
    ///
    /// Creates a new pool with default settings. For production use,
    /// prefer [`from_pool`](Self::from_pool) to share a tuned pool.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("database connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Create a session service from an existing connection pool.
    ///
    /// Use this to share a pool with tuned settings (max connections,
    /// idle timeout, etc.) across multiple services.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use sqlx::postgres::PgPoolOptions;
    ///
    /// let pool = PgPoolOptions::new()
    ///     .max_connections(20)
    ///     .min_connections(5)
    ///     .idle_timeout(std::time::Duration::from_secs(300))
    ///     .connect("postgres://user:pass@localhost/mydb")
    ///     .await?;
    ///
    /// let service = PostgresSessionService::from_pool(pool);
    /// ```
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The registry table used to track applied migration versions.
    const REGISTRY_TABLE: &'static str = "_adk_session_migrations";

    /// Advisory lock key derived from the registry table name.
    ///
    /// This is a fixed `i64` used with `pg_advisory_lock` /
    /// `pg_advisory_unlock` to prevent concurrent migration races.
    /// The value is a simple hash of the registry table name bytes.
    const ADVISORY_LOCK_KEY: i64 = {
        // Simple FNV-1a-style hash of "_adk_session_migrations" at compile time
        let bytes = Self::REGISTRY_TABLE.as_bytes();
        let mut hash: u64 = 0xcbf29ce484222325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        hash as i64
    };

    /// Compiled-in migration steps for the PostgreSQL session backend.
    ///
    /// Each entry is `(version, description, sql)`. Version 1 is the baseline
    /// that creates the initial schema with PostgreSQL-native types (`JSONB`,
    /// `TIMESTAMPTZ`) and indexes for common query patterns.
    const PG_SESSION_MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[(
        1,
        "create initial session tables",
        "\
CREATE TABLE IF NOT EXISTS sessions (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    state JSONB NOT NULL DEFAULT '{}', \
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
    PRIMARY KEY (app_name, user_id, session_id)\
);\
CREATE TABLE IF NOT EXISTS events (\
    id TEXT NOT NULL, \
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    invocation_id TEXT NOT NULL, \
    branch TEXT NOT NULL, \
    author TEXT NOT NULL, \
    timestamp TIMESTAMPTZ NOT NULL, \
    llm_response JSONB NOT NULL, \
    actions JSONB NOT NULL, \
    long_running_tool_ids JSONB NOT NULL, \
    PRIMARY KEY (id, app_name, user_id, session_id), \
    FOREIGN KEY (app_name, user_id, session_id) \
        REFERENCES sessions(app_name, user_id, session_id) \
        ON DELETE CASCADE\
);\
CREATE TABLE IF NOT EXISTS app_states (\
    app_name TEXT PRIMARY KEY, \
    state JSONB NOT NULL DEFAULT '{}', \
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()\
);\
CREATE TABLE IF NOT EXISTS user_states (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    state JSONB NOT NULL DEFAULT '{}', \
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
    PRIMARY KEY (app_name, user_id)\
);\
CREATE INDEX IF NOT EXISTS idx_sessions_app_user ON sessions(app_name, user_id);\
CREATE INDEX IF NOT EXISTS idx_events_session_ts ON events(session_id, timestamp);",
    )];

    /// Create the required tables and indexes if they do not exist.
    ///
    /// Tables created: `sessions`, `events`, `app_states`, `user_states`.
    /// Uses PostgreSQL-native types (`JSONB`, `TIMESTAMPTZ`) and standard
    /// foreign key constraints with `ON DELETE CASCADE`.
    ///
    /// Migrations are protected by a PostgreSQL advisory lock to prevent
    /// concurrent migration races from multiple application instances.
    pub async fn migrate(&self) -> Result<()> {
        let pool = &self.pool;

        // Acquire advisory lock to prevent concurrent migration races
        sqlx::query(&format!("SELECT pg_advisory_lock({})", Self::ADVISORY_LOCK_KEY))
            .execute(pool)
            .await
            .map_err(|e| {
                adk_core::AdkError::Session(format!("advisory lock acquisition failed: {e}"))
            })?;

        let result = crate::migration::pg_runner::run_sql_migrations(
            pool,
            Self::REGISTRY_TABLE,
            Self::PG_SESSION_MIGRATIONS,
            || async {
                let row = sqlx::query(
                    "SELECT EXISTS(\
                         SELECT 1 FROM information_schema.tables \
                         WHERE table_name = 'sessions'\
                     ) AS exists_flag",
                )
                .fetch_one(pool)
                .await
                .map_err(|e| {
                    adk_core::AdkError::Session(format!("baseline detection failed: {e}"))
                })?;
                let exists: bool = row.try_get("exists_flag").unwrap_or(false);
                Ok(exists)
            },
        )
        .await;

        // Release advisory lock regardless of migration outcome
        let _ = sqlx::query(&format!("SELECT pg_advisory_unlock({})", Self::ADVISORY_LOCK_KEY))
            .execute(pool)
            .await;

        result
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        crate::migration::pg_runner::sql_schema_version(&self.pool, Self::REGISTRY_TABLE).await
    }
}

#[async_trait]
impl SessionService for PostgresSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        // Upsert app state
        let app_state: HashMap<String, Value> =
            sqlx::query("SELECT state FROM app_states WHERE app_name = $1")
                .bind(&req.app_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
                .map(|row| {
                    row.get::<Value, _>("state")
                        .as_object()
                        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);

        let app_state_value = serde_json::to_value(&new_app_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO app_states (app_name, state, updated_at)
               VALUES ($1, $2, $3)
               ON CONFLICT (app_name) DO UPDATE SET state = $2, updated_at = $3"#,
        )
        .bind(&req.app_name)
        .bind(&app_state_value)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Upsert user state
        let user_state: HashMap<String, Value> =
            sqlx::query("SELECT state FROM user_states WHERE app_name = $1 AND user_id = $2")
                .bind(&req.app_name)
                .bind(&req.user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
                .map(|row| {
                    row.get::<Value, _>("state")
                        .as_object()
                        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);

        let user_state_value = serde_json::to_value(&new_user_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO user_states (app_name, user_id, state, updated_at)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (app_name, user_id) DO UPDATE SET state = $3, updated_at = $4"#,
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&user_state_value)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Create session with merged state
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_value = serde_json::to_value(&merged_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO sessions (app_name, user_id, session_id, state, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&session_id)
        .bind(&merged_state_value)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(Box::new(PostgresSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: merged_state,
            events: Vec::new(),
            updated_at: now,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let row = sqlx::query(
            "SELECT state, updated_at FROM sessions WHERE app_name = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| adk_core::AdkError::Session("session not found".into()))?;

        let state: HashMap<String, Value> = row
            .get::<Value, _>("state")
            .as_object()
            .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let updated_at: DateTime<Utc> = row.get("updated_at");

        let mut events: Vec<Event> = sqlx::query(
            "SELECT * FROM events WHERE app_name = $1 AND user_id = $2 AND session_id = $3 ORDER BY timestamp",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
        .into_iter()
        .filter_map(|row| {
            let llm_response_val: Value = row.get("llm_response");
            let actions_val: Value = row.get("actions");
            let tool_ids_val: Value = row.get("long_running_tool_ids");
            let llm_response = serde_json::from_value(llm_response_val).ok()?;
            let actions = serde_json::from_value(actions_val).ok()?;
            let long_running_tool_ids = serde_json::from_value(tool_ids_val).ok()?;
            let timestamp: DateTime<Utc> = row.get("timestamp");
            Some(Event {
                id: row.get("id"),
                timestamp,
                invocation_id: row.get("invocation_id"),
                branch: row.get("branch"),
                author: row.get("author"),
                llm_request: None,
                llm_response,
                actions,
                long_running_tool_ids,
                provider_metadata: std::collections::HashMap::new(),
            })
        })
        .collect();

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(PostgresSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state,
            events,
            updated_at,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let limit = req.limit.unwrap_or(i64::MAX as usize) as i64;
        let offset = req.offset.unwrap_or(0) as i64;

        let rows = sqlx::query(
            "SELECT session_id, state, updated_at FROM sessions \
             WHERE app_name = $1 AND user_id = $2 \
             ORDER BY updated_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?;

        let mut sessions = Vec::new();
        for row in rows {
            let state: HashMap<String, Value> = row
                .get::<Value, _>("state")
                .as_object()
                .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let updated_at: DateTime<Utc> = row.get("updated_at");

            sessions.push(Box::new(PostgresSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: row.get("session_id"),
                state,
                events: Vec::new(),
                updated_at,
            }) as Box<dyn Session>);
        }

        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        // CASCADE handles events deletion automatically in PostgreSQL
        sqlx::query(
            "DELETE FROM sessions WHERE app_name = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("delete failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {e}")))?;

        let session_rows =
            sqlx::query("SELECT app_name, user_id, state FROM sessions WHERE session_id = $1")
                .bind(session_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?;

        if session_rows.is_empty() {
            return Err(adk_core::AdkError::Session("session not found".into()));
        }
        if session_rows.len() > 1 {
            return Err(adk_core::AdkError::Session(format!(
                "ambiguous session_id '{session_id}'; expected a unique session identifier"
            )));
        }

        let row = &session_rows[0];
        let app_name: String = row.get("app_name");
        let user_id: String = row.get("user_id");
        let existing_state: HashMap<String, Value> = row
            .get::<Value, _>("state")
            .as_object()
            .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let (_, _, mut session_state) = state_utils::extract_state_deltas(&existing_state);

        // Load current app state
        let app_state: HashMap<String, Value> =
            match sqlx::query("SELECT state FROM app_states WHERE app_name = $1")
                .bind(&app_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            {
                Some(row) => row
                    .get::<Value, _>("state")
                    .as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default(),
                None => HashMap::new(),
            };

        // Load current user state
        let user_state: HashMap<String, Value> =
            match sqlx::query("SELECT state FROM user_states WHERE app_name = $1 AND user_id = $2")
                .bind(&app_name)
                .bind(&user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {e}")))?
            {
                Some(row) => row
                    .get::<Value, _>("state")
                    .as_object()
                    .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default(),
                None => HashMap::new(),
            };

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        // Update app state
        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);
        let app_state_value = serde_json::to_value(&new_app_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO app_states (app_name, state, updated_at)
               VALUES ($1, $2, $3)
               ON CONFLICT (app_name) DO UPDATE SET state = $2, updated_at = $3"#,
        )
        .bind(&app_name)
        .bind(&app_state_value)
        .bind(event.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update user state
        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);
        let user_state_value = serde_json::to_value(&new_user_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO user_states (app_name, user_id, state, updated_at)
               VALUES ($1, $2, $3, $4)
               ON CONFLICT (app_name, user_id) DO UPDATE SET state = $3, updated_at = $4"#,
        )
        .bind(&app_name)
        .bind(&user_id)
        .bind(&user_state_value)
        .bind(event.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        // Update session merged state
        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_value = serde_json::to_value(&merged_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            "UPDATE sessions SET state = $1, updated_at = $2 WHERE app_name = $3 AND user_id = $4 AND session_id = $5",
        )
        .bind(&merged_state_value)
        .bind(event.timestamp)
        .bind(&app_name)
        .bind(&user_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("update failed: {e}")))?;

        // Insert event
        let llm_response_value = serde_json::to_value(&event.llm_response)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let actions_value = serde_json::to_value(&event.actions)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let tool_ids_value = serde_json::to_value(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;

        sqlx::query(
            r#"INSERT INTO events (id, app_name, user_id, session_id, invocation_id, branch, author, timestamp, llm_response, actions, long_running_tool_ids)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
        )
        .bind(&event.id)
        .bind(&app_name)
        .bind(&user_id)
        .bind(session_id)
        .bind(&event.invocation_id)
        .bind(&event.branch)
        .bind(&event.author)
        .bind(event.timestamp)
        .bind(&llm_response_value)
        .bind(&actions_value)
        .bind(&tool_ids_value)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        // CASCADE handles events deletion automatically
        sqlx::query("DELETE FROM sessions WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete_all_sessions failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

struct PostgresSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for PostgresSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for PostgresSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        self.state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for PostgresSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}
