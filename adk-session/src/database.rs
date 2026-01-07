use crate::{
    CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_APP, KEY_PREFIX_TEMP,
    KEY_PREFIX_USER, ListRequest, Session, SessionService, State,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Row, sqlite::SqlitePool};
use std::collections::HashMap;
use uuid::Uuid;

pub struct DatabaseSessionService {
    pool: SqlitePool,
}

impl DatabaseSessionService {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url).await.map_err(|e| {
            adk_core::AdkError::Session(format!("database connection failed: {}", e))
        })?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                app_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (app_name, user_id, session_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("migration failed: {}", e)))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS events (
                id TEXT NOT NULL,
                app_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                invocation_id TEXT NOT NULL,
                branch TEXT NOT NULL,
                author TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                llm_response TEXT NOT NULL,
                actions TEXT NOT NULL,
                long_running_tool_ids TEXT NOT NULL,
                PRIMARY KEY (id, app_name, user_id, session_id),
                FOREIGN KEY (app_name, user_id, session_id) REFERENCES sessions(app_name, user_id, session_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("migration failed: {}", e)))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS app_states (
                app_name TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("migration failed: {}", e)))?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS user_states (
                app_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                state TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (app_name, user_id)
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("migration failed: {}", e)))?;

        Ok(())
    }

    fn extract_state_deltas(
        delta: &HashMap<String, Value>,
    ) -> (HashMap<String, Value>, HashMap<String, Value>, HashMap<String, Value>) {
        let mut app_delta = HashMap::new();
        let mut user_delta = HashMap::new();
        let mut session_delta = HashMap::new();

        for (key, value) in delta {
            if let Some(clean_key) = key.strip_prefix(KEY_PREFIX_APP) {
                app_delta.insert(clean_key.to_string(), value.clone());
            } else if let Some(clean_key) = key.strip_prefix(KEY_PREFIX_USER) {
                user_delta.insert(clean_key.to_string(), value.clone());
            } else if !key.starts_with(KEY_PREFIX_TEMP) {
                session_delta.insert(key.clone(), value.clone());
            }
        }

        (app_delta, user_delta, session_delta)
    }

    fn merge_states(
        app: &HashMap<String, Value>,
        user: &HashMap<String, Value>,
        session: &HashMap<String, Value>,
    ) -> HashMap<String, Value> {
        let mut merged = session.clone();
        for (k, v) in app {
            merged.insert(format!("{}{}", KEY_PREFIX_APP, k), v.clone());
        }
        for (k, v) in user {
            merged.insert(format!("{}{}", KEY_PREFIX_USER, k), v.clone());
        }
        merged
    }
}

#[async_trait]
impl SessionService for DatabaseSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = Self::extract_state_deltas(&req.state);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("transaction failed: {}", e)))?;

        // Get or create app state
        let app_state: HashMap<String, Value> =
            sqlx::query("SELECT state FROM app_states WHERE app_name = ?")
                .bind(&req.app_name)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {}", e)))?
                .map(|row| {
                    serde_json::from_str::<HashMap<String, Value>>(row.get("state"))
                        .unwrap_or_default()
                })
                .unwrap_or_default();

        let mut new_app_state = app_state.clone();
        new_app_state.extend(app_delta);

        let app_state_json = serde_json::to_string(&new_app_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;

        sqlx::query(
            "INSERT OR REPLACE INTO app_states (app_name, state, updated_at) VALUES (?, ?, ?)",
        )
        .bind(&req.app_name)
        .bind(&app_state_json)
        .bind(now.to_rfc3339())
        .execute(&mut *tx)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {}", e)))?;

        // Get or create user state
        let user_state: HashMap<String, Value> =
            sqlx::query("SELECT state FROM user_states WHERE app_name = ? AND user_id = ?")
                .bind(&req.app_name)
                .bind(&req.user_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("query failed: {}", e)))?
                .map(|row| {
                    serde_json::from_str::<HashMap<String, Value>>(row.get("state"))
                        .unwrap_or_default()
                })
                .unwrap_or_default();

        let mut new_user_state = user_state.clone();
        new_user_state.extend(user_delta);

        let user_state_json = serde_json::to_string(&new_user_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;

        sqlx::query("INSERT OR REPLACE INTO user_states (app_name, user_id, state, updated_at) VALUES (?, ?, ?, ?)")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&user_state_json)
            .bind(now.to_rfc3339())
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {}", e)))?;

        // Create session
        let merged_state = Self::merge_states(&new_app_state, &new_user_state, &session_state);
        let merged_state_json = serde_json::to_string(&merged_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;

        sqlx::query("INSERT INTO sessions (app_name, user_id, session_id, state, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&session_id)
            .bind(&merged_state_json)
            .bind(now.to_rfc3339())
            .bind(now.to_rfc3339())
            .execute(&mut *tx)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("commit failed: {}", e)))?;

        Ok(Box::new(DatabaseSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: merged_state,
            events: Vec::new(),
            updated_at: now,
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let row = sqlx::query("SELECT state, updated_at FROM sessions WHERE app_name = ? AND user_id = ? AND session_id = ?")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.session_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|_| adk_core::AdkError::Session("session not found".into()))?;

        let state: HashMap<String, Value> = serde_json::from_str(row.get("state"))
            .map_err(|e| adk_core::AdkError::Session(format!("deserialize failed: {}", e)))?;
        let updated_at: String = row.get("updated_at");
        let updated_at = DateTime::parse_from_rfc3339(&updated_at)
            .map_err(|e| adk_core::AdkError::Session(format!("parse date failed: {}", e)))?
            .with_timezone(&Utc);

        let events: Vec<Event> = sqlx::query("SELECT * FROM events WHERE app_name = ? AND user_id = ? AND session_id = ? ORDER BY timestamp")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.session_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("query failed: {}", e)))?
            .into_iter()
            .filter_map(|row| {
                let llm_response = serde_json::from_str(row.get("llm_response")).ok()?;
                let actions = serde_json::from_str(row.get("actions")).ok()?;
                let long_running_tool_ids = serde_json::from_str(row.get("long_running_tool_ids")).ok()?;
                let timestamp: String = row.get("timestamp");
                let timestamp = DateTime::parse_from_rfc3339(&timestamp).ok()?.with_timezone(&Utc);
                Some(Event {
                    id: row.get("id"),
                    timestamp,
                    invocation_id: row.get("invocation_id"),
                    invocation_id_camel: row.get("invocation_id"),
                    branch: row.get("branch"),
                    author: row.get("author"),
                    llm_request: None,
                    llm_response,
                    actions,
                    long_running_tool_ids,
                    gcp_llm_request: None,
                    gcp_llm_response: None,
                })
            })
            .collect();

        let mut events = events;

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

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
        let rows = sqlx::query(
            "SELECT session_id, state, updated_at FROM sessions WHERE app_name = ? AND user_id = ?",
        )
        .bind(&req.app_name)
        .bind(&req.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Session(format!("query failed: {}", e)))?;

        let mut sessions = Vec::new();
        for row in rows {
            let state: HashMap<String, Value> = serde_json::from_str(row.get("state"))
                .unwrap_or_default();
            let updated_at: String = row.get("updated_at");
            let updated_at = DateTime::parse_from_rfc3339(&updated_at)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            sessions.push(Box::new(DatabaseSession {
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

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE app_name = ? AND user_id = ? AND session_id = ?")
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(&req.session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("delete failed: {}", e)))?;

        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let llm_response_json = serde_json::to_string(&event.llm_response)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;
        let actions_json = serde_json::to_string(&event.actions)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;
        let tool_ids_json = serde_json::to_string(&event.long_running_tool_ids)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {}", e)))?;

        sqlx::query("INSERT INTO events (id, app_name, user_id, session_id, invocation_id, branch, author, timestamp, llm_response, actions, long_running_tool_ids) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&event.id)
            .bind("") // app_name - need to fetch from session
            .bind("") // user_id - need to fetch from session
            .bind(session_id)
            .bind(&event.invocation_id)
            .bind(&event.branch)
            .bind(&event.author)
            .bind(event.timestamp.to_rfc3339())
            .bind(&llm_response_json)
            .bind(&actions_json)
            .bind(&tool_ids_json)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("insert failed: {}", e)))?;

        Ok(())
    }
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
