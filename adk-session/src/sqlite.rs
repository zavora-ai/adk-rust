use crate::service::{
    ambiguous_session_id, identity_not_found, rewind_event_not_found, rewind_steps_exceed,
    session_already_exists, session_id_not_found, session_not_found_for, validate_scope,
    validate_session_id,
};
use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::Result;
use adk_core::identity::AdkIdentity;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqliteConnection, SqlitePool, SqliteRow},
};
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

/// Decodes a stored event row, logging and skipping a row that no longer deserializes
/// so one corrupt event does not hide the rest of the session history.
fn decode_event_row(row: &SqliteRow, session_id: &str) -> Option<Event> {
    fn decode(row: &SqliteRow) -> std::result::Result<Event, String> {
        let llm_response =
            serde_json::from_str(row.try_get("llm_response").map_err(|e| e.to_string())?)
                .map_err(|e| format!("llm_response: {e}"))?;
        let actions = serde_json::from_str(row.try_get("actions").map_err(|e| e.to_string())?)
            .map_err(|e| format!("actions: {e}"))?;
        let long_running_tool_ids =
            serde_json::from_str(row.try_get("long_running_tool_ids").map_err(|e| e.to_string())?)
                .map_err(|e| format!("long_running_tool_ids: {e}"))?;
        let timestamp: String = row.try_get("timestamp").map_err(|e| e.to_string())?;
        let timestamp = DateTime::parse_from_rfc3339(&timestamp)
            .map_err(|e| format!("timestamp: {e}"))?
            .with_timezone(&Utc);
        Ok(Event {
            id: row.try_get("id").map_err(|e| e.to_string())?,
            timestamp,
            invocation_id: row.try_get("invocation_id").map_err(|e| e.to_string())?,
            branch: row.try_get("branch").map_err(|e| e.to_string())?,
            author: row.try_get("author").map_err(|e| e.to_string())?,
            llm_request: None,
            llm_response,
            actions,
            long_running_tool_ids,
            provider_metadata: HashMap::new(),
        })
    }

    match decode(row) {
        Ok(event) => Some(event),
        Err(error) => {
            let event_id: String = row.try_get("id").unwrap_or_else(|_| "<unknown>".to_string());
            tracing::warn!(
                session.id = %session_id,
                event.id = %event_id,
                error = %error,
                "skipping stored event that failed to deserialize"
            );
            None
        }
    }
}

/// SQLite-backed session service using `sqlx`.
pub struct SqliteSessionService {
    pool: SqlitePool,
}

impl SqliteSessionService {
    /// Connect to SQLite and create a connection pool.
    ///
    /// Enables foreign keys via `PRAGMA foreign_keys = ON`. A writer waits up to 30 seconds
    /// for the database lock, so concurrent appends to a file database queue behind
    /// `BEGIN IMMEDIATE` instead of failing with `database is locked`.
    pub async fn new(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| adk_core::AdkError::session(format!("invalid sqlite url: {e}")))?
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("database connection failed: {e}")))?;
        sqlx::query("PRAGMA foreign_keys = ON").execute(&pool).await.map_err(|e| {
            adk_core::AdkError::session(format!("failed to enable sqlite foreign keys: {}", e))
        })?;
        Ok(Self { pool })
    }

    /// Create a session service from an existing connection pool.
    ///
    /// Use this to share a pool with tuned settings across multiple
    /// services, or in tests where you need direct pool access.
    ///
    /// **Note:** The caller is responsible for enabling foreign keys
    /// (`PRAGMA foreign_keys = ON`) on the pool if needed.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// The registry table used to track applied migration versions.
    const REGISTRY_TABLE: &'static str = "_adk_session_migrations";

    /// Compiled-in migration steps for the SQLite session backend.
    ///
    /// Each entry is `(version, description, sql)`. Version 1 is the baseline
    /// that creates the initial schema matching the original `CREATE TABLE IF
    /// NOT EXISTS` statements. Version 2 adds `sessions.initial_state`, the session state a
    /// session was created with, which rewind rebuilds from; it is `NULL` for sessions created
    /// before it.
    const SQLITE_SESSION_MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[
        (
            1,
            "create initial session tables",
            "\
CREATE TABLE IF NOT EXISTS sessions (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    state TEXT NOT NULL, \
    created_at TEXT NOT NULL, \
    updated_at TEXT NOT NULL, \
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
    timestamp TEXT NOT NULL, \
    llm_response TEXT NOT NULL, \
    actions TEXT NOT NULL, \
    long_running_tool_ids TEXT NOT NULL, \
    PRIMARY KEY (id, app_name, user_id, session_id), \
    FOREIGN KEY (app_name, user_id, session_id) \
        REFERENCES sessions(app_name, user_id, session_id) \
        ON DELETE CASCADE\
);\
CREATE TABLE IF NOT EXISTS app_states (\
    app_name TEXT PRIMARY KEY, \
    state TEXT NOT NULL, \
    updated_at TEXT NOT NULL\
);\
CREATE TABLE IF NOT EXISTS user_states (\
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    state TEXT NOT NULL, \
    updated_at TEXT NOT NULL, \
    PRIMARY KEY (app_name, user_id)\
);",
        ),
        (
            2,
            "record the session state each session was created with",
            "ALTER TABLE sessions ADD COLUMN initial_state TEXT;",
        ),
    ];

    /// Run all pending schema migrations for this backend.
    pub async fn migrate(&self) -> Result<()> {
        let pool = &self.pool;
        crate::migration::sqlite_runner::run_sql_migrations(
            pool,
            Self::REGISTRY_TABLE,
            Self::SQLITE_SESSION_MIGRATIONS,
            || async {
                let row = sqlx::query(
                    "SELECT COUNT(*) AS cnt FROM sqlite_master \
                     WHERE type='table' AND name='sessions'",
                )
                .fetch_one(pool)
                .await
                .map_err(|e| {
                    adk_core::AdkError::session(format!("baseline detection failed: {e}"))
                })?;
                let count: i64 = row.try_get("cnt").unwrap_or(0);
                Ok(count > 0)
            },
        )
        .await
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
        crate::migration::sqlite_runner::sql_schema_version(&self.pool, Self::REGISTRY_TABLE).await
    }
}

/// How long a connection waits for the database lock before `database is locked`.
///
/// Concurrent appends to a file database queue behind [`BEGIN_WRITE`]; sqlx's default of
/// five seconds is shorter than that queue on a slow disk.
const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Statement that opens every transaction which reads state and writes it back.
///
/// `BEGIN IMMEDIATE` takes the database write lock before the first read, so a concurrent
/// writer waits for the commit instead of overwriting a delta it never read.
const BEGIN_WRITE: &str = "BEGIN IMMEDIATE";

type StateMap = HashMap<String, Value>;

/// Decodes a stored state column; a missing row is empty state.
fn decode_state(json: Option<&str>) -> Result<StateMap> {
    match json {
        Some(json) => serde_json::from_str(json)
            .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}"))),
        None => Ok(HashMap::new()),
    }
}

fn encode_state(state: &StateMap) -> Result<String> {
    serde_json::to_string(state)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))
}

/// Reads the current app and user state tiers.
async fn read_tiers(
    conn: &mut SqliteConnection,
    app_name: &str,
    user_id: &str,
) -> Result<(StateMap, StateMap)> {
    let app_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM app_states WHERE app_name = ?")
            .bind(app_name)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
    let user_state: Option<String> =
        sqlx::query_scalar("SELECT state FROM user_states WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
    Ok((decode_state(app_state.as_deref())?, decode_state(user_state.as_deref())?))
}

/// Merges each non-empty tier delta into the stored tier and returns both tiers after
/// the write.
///
/// Runs inside a [`BEGIN_WRITE`] transaction, so no other writer commits between the read
/// and the write.
async fn apply_tier_deltas(
    conn: &mut SqliteConnection,
    app_name: &str,
    user_id: &str,
    app_delta: StateMap,
    user_delta: StateMap,
    now: DateTime<Utc>,
) -> Result<(StateMap, StateMap)> {
    let (mut app_state, mut user_state) = read_tiers(conn, app_name, user_id).await?;

    if !app_delta.is_empty() {
        app_state.extend(app_delta);
        sqlx::query(
            "INSERT OR REPLACE INTO app_states (app_name, state, updated_at) VALUES (?, ?, ?)",
        )
        .bind(app_name)
        .bind(encode_state(&app_state)?)
        .bind(now.to_rfc3339())
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;
    }

    if !user_delta.is_empty() {
        user_state.extend(user_delta);
        sqlx::query(
            "INSERT OR REPLACE INTO user_states (app_name, user_id, state, updated_at) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(encode_state(&user_state)?)
        .bind(now.to_rfc3339())
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;
    }

    Ok((app_state, user_state))
}

/// Applies `event` to the session `(app_name, user_id, session_id)` inside a
/// [`BEGIN_WRITE`] transaction: merges each tier's delta, bumps `updated_at`, and inserts
/// the event.
async fn apply_event(
    conn: &mut SqliteConnection,
    app_name: &str,
    user_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<()> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT state FROM sessions WHERE app_name = ? AND user_id = ? AND session_id = ?",
    )
    .bind(app_name)
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
    let Some(stored) = stored else {
        return Err(session_not_found_for(app_name, user_id, session_id));
    };

    let (app_delta, user_delta, session_delta) =
        state_utils::extract_state_deltas(&event.actions.state_delta);
    apply_tier_deltas(conn, app_name, user_id, app_delta, user_delta, event.timestamp).await?;

    if session_delta.is_empty() {
        sqlx::query(
            "UPDATE sessions SET updated_at = ? WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(event.timestamp.to_rfc3339())
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    } else {
        // Rows written by earlier releases also hold a copy of the app and user tiers; the
        // rewrite keeps only the session tier.
        let (_, _, mut session_state) =
            state_utils::extract_state_deltas(&decode_state(Some(&stored))?);
        session_state.extend(session_delta);
        sqlx::query(
            "UPDATE sessions SET state = ?, updated_at = ? \
             WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(encode_state(&session_state)?)
        .bind(event.timestamp.to_rfc3339())
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
    }

    let llm_response_json = serde_json::to_string(&event.llm_response)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let actions_json = serde_json::to_string(&event.actions)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let tool_ids_json = serde_json::to_string(&event.long_running_tool_ids)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;

    sqlx::query(
        "INSERT INTO events (id, app_name, user_id, session_id, invocation_id, branch, author, \
         timestamp, llm_response, actions, long_running_tool_ids) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(app_name)
    .bind(user_id)
    .bind(session_id)
    .bind(&event.invocation_id)
    .bind(&event.branch)
    .bind(&event.author)
    .bind(event.timestamp.to_rfc3339())
    .bind(&llm_response_json)
    .bind(&actions_json)
    .bind(&tool_ids_json)
    .execute(&mut *conn)
    .await
    .map_err(|e| adk_core::AdkError::session(format!("insert failed: {e}")))?;

    Ok(())
}

#[async_trait]
impl SessionService for SqliteSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let mut tx = self
            .pool
            .begin_with(BEGIN_WRITE)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        let (app_state, user_state) =
            apply_tier_deltas(&mut tx, &req.app_name, &req.user_id, app_delta, user_delta, now)
                .await?;

        let encoded = encode_state(&session_state)?;
        // A conflicting insert fails the transaction, which rolls back the tier writes above.
        sqlx::query(
            "INSERT INTO sessions \
             (app_name, user_id, session_id, state, initial_state, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&session_id)
        .bind(&encoded)
        .bind(&encoded)
        .bind(now.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                session_already_exists(&req.app_name, &req.user_id, &session_id)
            }
            other => adk_core::AdkError::session(format!("insert failed: {other}")),
        })?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(Box::new(DatabaseSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: state_utils::merge_states(&app_state, &user_state, &session_state),
            events: Vec::new(),
            updated_at: now,
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        let row = sqlx::query(
            "SELECT s.state, s.updated_at, a.state AS app_state, u.state AS user_state \
             FROM sessions s \
             LEFT JOIN app_states a ON a.app_name = s.app_name \
             LEFT JOIN user_states u ON u.app_name = s.app_name AND u.user_id = s.user_id \
             WHERE s.app_name = ? AND s.user_id = ? AND s.session_id = ?",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .ok_or_else(|| crate::service::session_not_found(&req))?;

        let state = state_utils::merge_current_tiers(
            &decode_state(row.get("app_state"))?,
            &decode_state(row.get("user_state"))?,
            &decode_state(Some(row.get("state")))?,
        );
        let updated_at: String = row.get("updated_at");
        let updated_at = DateTime::parse_from_rfc3339(&updated_at)
            .map_err(|e| adk_core::AdkError::session(format!("parse date failed: {}", e)))?
            .with_timezone(&Utc);

        // Timestamps are RFC 3339 strings in one format, so text order is time order. The
        // inner query keeps the most recent `num_recent_events` (-1 is no limit); the outer
        // one restores chronological order.
        let after = req.after.map(|after| after.to_rfc3339());
        let limit = req.num_recent_events.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
        let events: Vec<Event> = sqlx::query(
            "SELECT * FROM (\
                 SELECT *, rowid AS seq FROM events \
                 WHERE app_name = ? AND user_id = ? AND session_id = ? \
                   AND (? IS NULL OR timestamp >= ?) \
                 ORDER BY timestamp DESC, rowid DESC LIMIT ?\
             ) ORDER BY timestamp, seq",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(&req.session_id)
        .bind(&after)
        .bind(&after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {}", e)))?
        .iter()
        .filter_map(|row| decode_event_row(row, &req.session_id))
        .collect();

        Ok(Box::new(DatabaseSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state,
            events,
            updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        validate_scope(&req.app_name, &req.user_id)?;
        let limit = req.limit.map(|l| l as i64).unwrap_or(i64::MAX);
        let offset = req.offset.unwrap_or(0) as i64;

        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                adk_core::AdkError::session(format!("database connection failed: {e}"))
            })?;
        let (app_state, user_state) = read_tiers(&mut conn, &req.app_name, &req.user_id).await?;

        let rows = sqlx::query(
            "SELECT session_id, state, updated_at FROM sessions \
             WHERE app_name = ? AND user_id = ? \
             ORDER BY updated_at DESC, session_id DESC LIMIT ? OFFSET ?",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {}", e)))?;

        let mut sessions = Vec::new();
        for row in rows {
            let stored: StateMap = serde_json::from_str(row.get("state")).unwrap_or_default();
            let updated_at: String = row.get("updated_at");
            let updated_at = DateTime::parse_from_rfc3339(&updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            sessions.push(Box::new(DatabaseSession {
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

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {}", e)))?;

        // Explicitly remove events first for deterministic cleanup across sqlite
        // configurations where foreign-key enforcement may differ.
        sqlx::query("DELETE FROM events WHERE app_name = ? AND user_id = ? AND session_id = ?")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.session_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("delete events failed: {}", e)))?;

        sqlx::query("DELETE FROM sessions WHERE app_name = ? AND user_id = ? AND session_id = ?")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.session_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("delete failed: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {}", e)))?;

        Ok(())
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {}", e)))?;

        sqlx::query("DELETE FROM events WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!("delete_all_sessions failed: {}", e))
            })?;

        sqlx::query("DELETE FROM sessions WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!("delete_all_sessions failed: {}", e))
            })?;

        sqlx::query("DELETE FROM user_states WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                adk_core::AdkError::session(format!("delete_all_sessions failed: {}", e))
            })?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {}", e)))?;

        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        validate_session_id(session_id)?;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut tx = self
            .pool
            .begin_with(BEGIN_WRITE)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {}", e)))?;

        let session_rows =
            sqlx::query("SELECT app_name, user_id FROM sessions WHERE session_id = ?")
                .bind(session_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::session(format!("query failed: {}", e)))?;

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
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {}", e)))?;

        Ok(())
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let mut tx = self
            .pool
            .begin_with(BEGIN_WRITE)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {}", e)))?;

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
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {}", e)))?;

        Ok(())
    }

    async fn rewind(
        &self,
        identity: &AdkIdentity,
        target_event_id: &str,
    ) -> Result<Box<dyn Session>> {
        self.truncate(identity, |events| {
            events
                .iter()
                .position(|event| event.id == target_event_id)
                .map(|index| index + 1)
                .ok_or_else(|| rewind_event_not_found(identity, target_event_id))
        })
        .await
    }

    async fn rewind_steps(&self, identity: &AdkIdentity, steps: usize) -> Result<Box<dyn Session>> {
        if steps == 0 {
            return self.get_for_identity(identity).await;
        }
        self.truncate(identity, |events| {
            events
                .len()
                .checked_sub(steps)
                .ok_or_else(|| rewind_steps_exceed(identity, steps, events.len()))
        })
        .await
    }
}

impl SqliteSessionService {
    /// Keeps the first `keep(events)` events of `identity`, in read order, and rebuilds its
    /// session tier from the state it was created with and their deltas, in one
    /// [`BEGIN_WRITE`] transaction.
    async fn truncate(
        &self,
        identity: &AdkIdentity,
        keep: impl FnOnce(&[Event]) -> Result<usize> + Send,
    ) -> Result<Box<dyn Session>> {
        let (app_name, user_id, session_id) =
            (identity.app_name.as_ref(), identity.user_id.as_ref(), identity.session_id.as_ref());
        let mut tx = self
            .pool
            .begin_with(BEGIN_WRITE)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        let row = sqlx::query(
            "SELECT state, initial_state FROM sessions \
             WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
        .ok_or_else(|| identity_not_found(identity))?;
        let stored = decode_state(Some(row.get("state")))?;
        let initial: Option<String> = row.get("initial_state");

        let rows = sqlx::query(
            "SELECT *, rowid AS seq FROM events \
             WHERE app_name = ? AND user_id = ? AND session_id = ? ORDER BY timestamp, rowid",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        let mut events = Vec::with_capacity(rows.len());
        let mut seqs = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(event) = decode_event_row(row, session_id) {
                seqs.push(row.get::<i64, _>("seq"));
                events.push(event);
            }
        }

        let initial = match initial {
            Some(initial) => decode_state(Some(&initial))?,
            None => legacy_initial_state(&stored, &events),
        };
        let keep = keep(&events)?;
        // Read order is (timestamp, rowid), so the kept events are those up to the last kept
        // row; every later row goes, including rows that no longer decode.
        match keep.checked_sub(1).map(|last| (&events[last], seqs[last])) {
            Some((last, seq)) => sqlx::query(
                "DELETE FROM events WHERE app_name = ? AND user_id = ? AND session_id = ? \
                     AND (timestamp > ? OR (timestamp = ? AND rowid > ?))",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(last.timestamp.to_rfc3339())
            .bind(last.timestamp.to_rfc3339())
            .bind(seq),
            None => sqlx::query(
                "DELETE FROM events WHERE app_name = ? AND user_id = ? AND session_id = ?",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id),
        }
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("delete events failed: {e}")))?;
        events.truncate(keep);

        let session_state = state_utils::rebuild_session_state(&initial, &events);
        let now = Utc::now();
        sqlx::query(
            "UPDATE sessions SET state = ?, updated_at = ? \
             WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(encode_state(&session_state)?)
        .bind(now.to_rfc3339())
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::session(format!("update failed: {e}")))?;
        let (app_state, user_state) = read_tiers(&mut tx, app_name, user_id).await?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(Box::new(DatabaseSession {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            state: state_utils::merge_states(&app_state, &user_state, &session_state),
            events,
            updated_at: now,
        }))
    }
}

/// The best reconstruction of the creation state of a session created before
/// `initial_state` was recorded: its session-tier keys that no event has written, whose values
/// cannot have changed since `create`.
fn legacy_initial_state(stored: &StateMap, events: &[Event]) -> StateMap {
    let (_, _, mut state) = state_utils::extract_state_deltas(stored);
    for event in events {
        for key in event.actions.state_delta.keys() {
            state.remove(key);
        }
    }
    state
}

struct DatabaseSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for DatabaseSession {
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

impl State for DatabaseSession {
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

impl Events for DatabaseSession {
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
