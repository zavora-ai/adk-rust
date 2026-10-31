use crate::service::{
    ambiguous_session_id, session_already_exists, session_id_not_found, session_not_found_for,
    validate_scope, validate_session_id,
};
use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row};
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
            .map_err(|e| adk_core::AdkError::session(format!("database connection failed: {e}")))?;
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

    /// Advisory lock key that [`migrate`](Self::migrate) holds while it runs.
    ///
    /// Concurrent instances take this `pg_advisory_lock` key so only one migrates at a
    /// time. The value is an FNV-1a hash of the registry table name, and `pg_locks` shows
    /// it with the high 32 bits in `classid` and the low 32 bits in `objid`.
    pub const ADVISORY_LOCK_KEY: i64 = {
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
    /// `TIMESTAMPTZ`) and indexes for common query patterns. Version 2 adds `events.seq`, an
    /// insertion sequence that orders events with equal timestamps; existing rows are numbered
    /// in their physical order.
    const PG_SESSION_MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[
        (
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
        ),
        (
            2,
            "order events with equal timestamps by insertion",
            "ALTER TABLE events ADD COLUMN IF NOT EXISTS seq BIGSERIAL;\
CREATE INDEX IF NOT EXISTS idx_events_session_order \
    ON events(app_name, user_id, session_id, timestamp, seq);",
        ),
    ];

    /// Create the required tables and indexes if they do not exist.
    ///
    /// Tables created: `sessions`, `events`, `app_states`, `user_states`.
    /// Uses PostgreSQL-native types (`JSONB`, `TIMESTAMPTZ`) and standard
    /// foreign key constraints with `ON DELETE CASCADE`.
    ///
    /// Migrations are protected by a PostgreSQL advisory lock to prevent
    /// concurrent migration races from multiple application instances. The
    /// lock, every migration statement, and the unlock run on one connection,
    /// which is closed afterwards instead of returning to the pool.
    pub async fn migrate(&self) -> Result<()> {
        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                adk_core::AdkError::session(format!("database connection failed: {e}"))
            })?;
        // A session-level advisory lock belongs to this connection. Closing it on
        // drop releases the lock even when this future is cancelled, rather than
        // leaving it held by an idle pooled connection.
        conn.close_on_drop();

        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(Self::ADVISORY_LOCK_KEY)
            .execute(&mut *conn)
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!("advisory lock acquisition failed: {e}"))
            })?;

        let result = crate::migration::pg_runner::run_sql_migrations_on_connection(
            &mut conn,
            Self::REGISTRY_TABLE,
            Self::PG_SESSION_MIGRATIONS,
            |conn| {
                Box::pin(async move {
                    let row = sqlx::query(
                        "SELECT EXISTS(\
                             SELECT 1 FROM information_schema.tables \
                             WHERE table_schema = current_schema() \
                               AND table_name = 'sessions'\
                         ) AS exists_flag",
                    )
                    .fetch_one(conn)
                    .await
                    .map_err(|e| {
                        adk_core::AdkError::session(format!("baseline detection failed: {e}"))
                    })?;
                    let exists: bool = row.try_get("exists_flag").unwrap_or(false);
                    Ok(exists)
                })
            },
        )
        .await;

        // Released explicitly so the next instance proceeds without waiting for
        // the connection to close; closing the connection releases it regardless.
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(Self::ADVISORY_LOCK_KEY)
            .execute(&mut *conn)
            .await;

        result
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    ///
    /// # Errors
    ///
    /// Returns an error when the registry cannot be read for any reason other
    /// than the table being absent, such as a lost connection or a missing
    /// privilege.
    pub async fn schema_version(&self) -> Result<i64> {
        crate::migration::pg_runner::sql_schema_version(&self.pool, Self::REGISTRY_TABLE).await
    }
}

type StateMap = HashMap<String, Value>;

/// Marks a string stored in escaped form.
///
/// `JSONB` cannot hold U+0000, so a string or object key that contains it is stored as this
/// prefix followed by the string with `%` written as `%25` and U+0000 as `%00`. A string that
/// already starts with the prefix is escaped the same way, so reading back is exact. The prefix
/// starts with a private-use character, and strings without U+0000 are stored unchanged.
const NUL_ESCAPE_PREFIX: &str = "\u{e000}adk-nul:";

fn needs_escape(text: &str) -> bool {
    text.contains('\0') || text.starts_with(NUL_ESCAPE_PREFIX)
}

fn escape_text(text: String) -> String {
    if !needs_escape(&text) {
        return text;
    }
    let mut escaped = String::with_capacity(NUL_ESCAPE_PREFIX.len() + text.len() + 8);
    escaped.push_str(NUL_ESCAPE_PREFIX);
    for c in text.chars() {
        match c {
            '%' => escaped.push_str("%25"),
            '\0' => escaped.push_str("%00"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn unescape_text(text: String) -> String {
    let Some(rest) = text.strip_prefix(NUL_ESCAPE_PREFIX) else { return text };
    let mut decoded = String::with_capacity(rest.len());
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            decoded.push(c);
            continue;
        }
        let code: String = chars.by_ref().take(2).collect();
        match code.as_str() {
            "25" => decoded.push('%'),
            "00" => decoded.push('\0'),
            other => {
                decoded.push('%');
                decoded.push_str(other);
            }
        }
    }
    decoded
}

/// Rewrites every string and object key of `value` with `rewrite`, in place.
fn rewrite_strings(value: &mut Value, rewrite: fn(String) -> String, applies: fn(&str) -> bool) {
    match value {
        Value::String(text) => {
            if applies(text) {
                *text = rewrite(std::mem::take(text));
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_strings(item, rewrite, applies);
            }
        }
        Value::Object(map) => {
            if map.keys().any(|key| applies(key)) {
                *map = std::mem::take(map)
                    .into_iter()
                    .map(|(key, value)| (if applies(&key) { rewrite(key) } else { key }, value))
                    .collect();
            }
            for item in map.values_mut() {
                rewrite_strings(item, rewrite, applies);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Serializes `value` for a `JSONB` column; see [`NUL_ESCAPE_PREFIX`].
fn to_jsonb<T: serde::Serialize + ?Sized>(value: &T) -> Result<Value> {
    let mut value = serde_json::to_value(value)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    rewrite_strings(&mut value, escape_text, needs_escape);
    Ok(value)
}

/// Reverses [`to_jsonb`] on a value read from a `JSONB` column.
fn from_jsonb(mut value: Value) -> Value {
    rewrite_strings(&mut value, unescape_text, |text| text.starts_with(NUL_ESCAPE_PREFIX));
    value
}

/// Converts a stored `JSONB` state object; a missing row or a non-object is empty state.
fn decode_state(value: Option<Value>) -> StateMap {
    match value.map(from_jsonb) {
        Some(Value::Object(map)) => map.into_iter().collect(),
        _ => HashMap::new(),
    }
}

fn encode_state(state: &StateMap) -> Result<Value> {
    to_jsonb(state)
}

/// Reads the current app and user state tiers.
async fn read_tiers(
    conn: &mut PgConnection,
    app_name: &str,
    user_id: &str,
) -> Result<(StateMap, StateMap)> {
    let app_state: Option<Value> =
        sqlx::query_scalar("SELECT state FROM app_states WHERE app_name = $1")
            .bind(app_name)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
    let user_state: Option<Value> =
        sqlx::query_scalar("SELECT state FROM user_states WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
    Ok((decode_state(app_state), decode_state(user_state)))
}

/// Merges each non-empty tier delta into the stored tier.
///
/// `state || delta` is evaluated against the latest committed row while holding its row
/// lock, so concurrent writers of different keys never overwrite each other.
async fn apply_tier_deltas(
    conn: &mut PgConnection,
    app_name: &str,
    user_id: &str,
    app_delta: &StateMap,
    user_delta: &StateMap,
    now: DateTime<Utc>,
) -> Result<()> {
    if !app_delta.is_empty() {
        sqlx::query(
            "INSERT INTO app_states (app_name, state, updated_at) VALUES ($1, $2, $3) \
             ON CONFLICT (app_name) DO UPDATE \
             SET state = app_states.state || EXCLUDED.state, updated_at = EXCLUDED.updated_at",
        )
        .bind(app_name)
        .bind(encode_state(app_delta)?)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;
    }

    if !user_delta.is_empty() {
        sqlx::query(
            "INSERT INTO user_states (app_name, user_id, state, updated_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (app_name, user_id) DO UPDATE \
             SET state = user_states.state || EXCLUDED.state, updated_at = EXCLUDED.updated_at",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(encode_state(user_delta)?)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;
    }

    Ok(())
}

/// Applies `event` to the session `(app_name, user_id, session_id)`: merges each tier's
/// delta, bumps `updated_at`, and inserts the event.
///
/// Row locks are taken session, app, user. Every writer takes the app row before the user
/// row and `create` locks no existing session row, so concurrent writers cannot deadlock.
async fn apply_event(
    conn: &mut PgConnection,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<()> {
    let (app_delta, user_delta, session_delta) =
        state_utils::extract_state_deltas(&event.actions.state_delta);

    let updated = sqlx::query(
        "UPDATE sessions SET state = state || $1, updated_at = $2 \
         WHERE app_name = $3 AND user_id = $4 AND session_id = $5",
    )
    .bind(encode_state(&session_delta)?)
    .bind(event.timestamp)
    .bind(app_name)
    .bind(user_id)
    .bind(session_id)
    .execute(&mut *conn)
    .await
    .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    if updated.rows_affected() == 0 {
        return Err(session_not_found_for(app_name, user_id, session_id));
    }

    apply_tier_deltas(conn, app_name, user_id, &app_delta, &user_delta, event.timestamp).await?;

    let llm_response_value = to_jsonb(&event.llm_response)?;
    let actions_value = to_jsonb(&event.actions)?;
    let tool_ids_value = to_jsonb(&event.long_running_tool_ids)?;

    sqlx::query(
        r#"INSERT INTO events (id, app_name, user_id, session_id, invocation_id, branch, author, timestamp, llm_response, actions, long_running_tool_ids)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(&event.id)
    .bind(app_name)
    .bind(user_id)
    .bind(session_id)
    .bind(&event.invocation_id)
    .bind(&event.branch)
    .bind(&event.author)
    .bind(event.timestamp)
    .bind(&llm_response_value)
    .bind(&actions_value)
    .bind(&tool_ids_value)
    .execute(&mut *conn)
    .await
    .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

    Ok(())
}

#[async_trait]
impl SessionService for PostgresSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        apply_tier_deltas(&mut tx, &req.app_name, &req.user_id, &app_delta, &user_delta, now)
            .await?;
        let (app_state, user_state) = read_tiers(&mut tx, &req.app_name, &req.user_id).await?;

        sqlx::query(
            r#"INSERT INTO sessions (app_name, user_id, session_id, state, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&session_id)
        .bind(encode_state(&session_state)?)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            // The failed insert aborts the transaction, which rolls back the tier writes above.
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                session_already_exists(&req.app_name, &req.user_id, &session_id)
            }
            other => adk_core::AdkError::session(format!("insert failed: {other}")),
        })?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(Box::new(PostgresSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: state_utils::merge_states(&app_state, &user_state, &session_state),
            events: Vec::new(),
            updated_at: now,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let row = sqlx::query(
            "SELECT s.state, s.updated_at, a.state AS app_state, u.state AS user_state \
             FROM sessions s \
             LEFT JOIN app_states a ON a.app_name = s.app_name \
             LEFT JOIN user_states u ON u.app_name = s.app_name AND u.user_id = s.user_id \
             WHERE s.app_name = $1 AND s.user_id = $2 AND s.session_id = $3",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .ok_or_else(|| crate::service::session_not_found(&req))?;

        let state = state_utils::merge_current_tiers(
            &decode_state(row.get("app_state")),
            &decode_state(row.get("user_state")),
            &decode_state(row.get("state")),
        );
        let updated_at: DateTime<Utc> = row.get("updated_at");

        // The inner query keeps the most recent `num_recent_events` (NULL is no limit); the
        // outer one restores chronological order.
        let limit = req.num_recent_events.map(|n| i64::try_from(n).unwrap_or(i64::MAX));
        let events: Vec<Event> = sqlx::query(
            "SELECT * FROM (\
                 SELECT * FROM events \
                 WHERE app_name = $1 AND user_id = $2 AND session_id = $3 \
                   AND ($4::timestamptz IS NULL OR timestamp >= $4) \
                 ORDER BY timestamp DESC, seq DESC LIMIT $5\
             ) recent ORDER BY timestamp, seq",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .bind(req.after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .into_iter()
        .filter_map(|row| {
            let event_id: String = row.get("id");
            let column = |name: &str| from_jsonb(row.get(name));
            let decoded = serde_json::from_value(column("llm_response")).and_then(|llm_response| {
                Ok((
                    llm_response,
                    serde_json::from_value(column("actions"))?,
                    serde_json::from_value(column("long_running_tool_ids"))?,
                ))
            });
            let (llm_response, actions, long_running_tool_ids) = match decoded {
                Ok(parts) => parts,
                Err(error) => {
                    tracing::warn!(
                        session.id = %req.session_id,
                        event.id = %event_id,
                        error = %error,
                        "skipping stored event that failed to deserialize"
                    );
                    return None;
                }
            };
            let timestamp: DateTime<Utc> = row.get("timestamp");
            Some(Event {
                id: event_id,
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
        validate_scope(&req.app_name, &req.user_id)?;
        let limit = req.limit.unwrap_or(i64::MAX as usize) as i64;
        let offset = req.offset.unwrap_or(0) as i64;

        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                adk_core::AdkError::session(format!("database connection failed: {e}"))
            })?;
        let (app_state, user_state) = read_tiers(&mut conn, &req.app_name, &req.user_id).await?;

        let rows = sqlx::query(
            "SELECT session_id, state, updated_at FROM sessions \
             WHERE app_name = $1 AND user_id = $2 \
             ORDER BY updated_at DESC, session_id DESC LIMIT $3 OFFSET $4",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut sessions = Vec::new();
        for row in rows {
            let stored = decode_state(row.get("state"));
            let updated_at: DateTime<Utc> = row.get("updated_at");

            sessions.push(Box::new(PostgresSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: row.get("session_id"),
                state: state_utils::merge_current_tiers(&app_state, &user_state, &stored),
                events: Vec::new(),
                updated_at,
            }) as Box<dyn Session>);
        }

        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        // CASCADE handles events deletion automatically in PostgreSQL
        sqlx::query(
            "DELETE FROM sessions WHERE app_name = $1 AND user_id = $2 AND session_id = $3",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        validate_session_id(session_id)?;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        let session_rows =
            sqlx::query("SELECT app_name, user_id FROM sessions WHERE session_id = $1")
                .bind(session_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        if session_rows.is_empty() {
            return Err(session_id_not_found(session_id));
        }
        if session_rows.len() > 1 {
            return Err(ambiguous_session_id(session_id));
        }

        let row = &session_rows[0];
        let app_name: String = row.get("app_name");
        let user_id: String = row.get("user_id");

        apply_event(&mut tx, &app_name, &user_id, session_id, &event).await?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(
        app_name = %req.identity.app_name,
        user_id = %req.identity.user_id,
        session_id = %req.identity.session_id,
    ))]
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        apply_event(
            &mut tx,
            req.identity.app_name.as_ref(),
            req.identity.user_id.as_ref(),
            req.identity.session_id.as_ref(),
            &event,
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;
        // CASCADE handles events deletion automatically
        sqlx::query("DELETE FROM sessions WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("delete_all_sessions failed: {e}")))?;
        sqlx::query("DELETE FROM user_states WHERE app_name = $1 AND user_id = $2")
            .bind(app_name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("delete_all_sessions failed: {e}")))?;
        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("health check failed: {e}")))?;
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
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jsonb_escaping_round_trips_nul_percent_and_the_prefix() {
        let original = json!({
            "plain": "no escape needed",
            "nul": "PK\u{0}\u{3}binary",
            "percent": "100% and %00 and \u{0}",
            "lookalike": format!("{NUL_ESCAPE_PREFIX}%00 literal"),
            "key\u{0}with nul": ["a\u{0}b", {"nested\u{0}": "c\u{0}"}],
            "numbers": [1, 2.5, null, true],
        });

        let stored = to_jsonb(&original).unwrap();
        assert!(!stored.to_string().contains("\\u0000"), "no U+0000 reaches JSONB: {stored}");
        assert_eq!(stored["plain"], json!("no escape needed"), "plain strings are stored as is");
        assert_eq!(from_jsonb(stored), original);
    }

    #[test]
    fn jsonb_reading_leaves_unescaped_strings_alone() {
        let legacy = json!({"text": "50% off", "odd": "%zz"});
        assert_eq!(from_jsonb(legacy.clone()), legacy);
    }
}
