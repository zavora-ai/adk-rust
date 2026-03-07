//! Redis-backed session service.
//!
//! Uses `fred` for async Redis operations with support for standalone
//! and cluster deployments. Sessions are stored as Redis hashes, events
//! as sorted sets (scored by timestamp), and state tiers as separate hashes.
//!
//! # Data Model
//!
//! | Key Pattern | Type | Contents |
//! |---|---|---|
//! | `{app}:{user}:{session}` | Hash | Session metadata + state |
//! | `{app}:{user}:{session}:events` | Sorted Set | Events scored by timestamp |
//! | `app_state:{app}` | Hash | App-level state |
//! | `user_state:{app}:{user}` | Hash | User-level state |
//! | `sessions_idx:{app}:{user}` | Set | Session IDs for list lookups |
//! | `session_lookup:{session}` | String | Reverse lookup to `{app}:{user}` |

use crate::{
    CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP, ListRequest, Session,
    SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fred::clients::Transaction;
use fred::prelude::*;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::instrument;
use uuid::Uuid;

/// Configuration for connecting to Redis.
#[derive(Debug, Clone)]
pub struct RedisSessionConfig {
    /// Redis connection URL (e.g. `redis://localhost:6379`).
    pub url: String,
    /// Optional TTL applied to session and events keys.
    pub ttl: Option<Duration>,
    /// Optional cluster node addresses for cluster mode.
    pub cluster_nodes: Option<Vec<String>>,
}

// --- Key generation functions ---

/// Session metadata hash key: `{app}:{user}:{session}`.
pub fn session_key(app: &str, user: &str, session: &str) -> String {
    format!("{app}:{user}:{session}")
}

/// Events sorted set key: `{app}:{user}:{session}:events`.
pub fn events_key(app: &str, user: &str, session: &str) -> String {
    format!("{app}:{user}:{session}:events")
}

/// App-level state hash key: `app_state:{app}`.
pub fn app_state_key(app: &str) -> String {
    format!("app_state:{app}")
}

/// User-level state hash key: `user_state:{app}:{user}`.
pub fn user_state_key(app: &str, user: &str) -> String {
    format!("user_state:{app}:{user}")
}

/// Session index set key: `sessions_idx:{app}:{user}`.
pub fn index_key(app: &str, user: &str) -> String {
    format!("sessions_idx:{app}:{user}")
}

/// Reverse lookup key: `session_lookup:{session}` → `{app}:{user}`.
fn lookup_key(session: &str) -> String {
    format!("session_lookup:{session}")
}

/// Redis-backed session service.
///
/// Stores sessions using Redis hashes, events as sorted sets scored by
/// timestamp, and three-tier state (app, user, session) as separate hashes.
/// Supports optional TTL on session keys and cluster mode.
///
/// # Example
///
/// ```rust,ignore
/// use adk_session::{RedisSessionConfig, RedisSessionService};
///
/// let config = RedisSessionConfig {
///     url: "redis://localhost:6379".into(),
///     ttl: None,
///     cluster_nodes: None,
/// };
/// let service = RedisSessionService::new(config).await?;
/// ```
pub struct RedisSessionService {
    client: Client,
    ttl: Option<Duration>,
}

impl RedisSessionService {
    /// Connect to Redis (standalone or cluster).
    ///
    /// Returns an error with "redis connection failed" context if the
    /// connection cannot be established.
    pub async fn new(config: RedisSessionConfig) -> Result<Self> {
        let redis_config = if let Some(ref nodes) = config.cluster_nodes {
            let hosts: Vec<(String, u16)> = nodes
                .iter()
                .map(|n| {
                    let parts: Vec<&str> = n.rsplitn(2, ':').collect();
                    if parts.len() == 2 {
                        let port = parts[0].parse::<u16>().unwrap_or(6379);
                        (parts[1].to_string(), port)
                    } else {
                        (n.clone(), 6379)
                    }
                })
                .collect();
            Config { server: ServerConfig::new_clustered(hosts), ..Default::default() }
        } else {
            Config::from_url(&config.url)
                .map_err(|e| adk_core::AdkError::Session(format!("redis connection failed: {e}")))?
        };

        let client = Builder::from_config(redis_config)
            .build()
            .map_err(|e| adk_core::AdkError::Session(format!("redis connection failed: {e}")))?;

        client
            .init()
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis connection failed: {e}")))?;

        Ok(Self { client, ttl: config.ttl })
    }

    /// Apply TTL to session-related keys if configured.
    async fn apply_ttl(&self, session_k: &str, events_k: &str) -> Result<()> {
        if let Some(ttl) = self.ttl {
            let seconds = ttl.as_secs() as i64;
            let _: () =
                self.client.expire(session_k, seconds, None).await.map_err(|e| {
                    adk_core::AdkError::Session(format!("redis expire failed: {e}"))
                })?;
            let _: () =
                self.client.expire(events_k, seconds, None).await.map_err(|e| {
                    adk_core::AdkError::Session(format!("redis expire failed: {e}"))
                })?;
        }
        Ok(())
    }

    /// Read a hash as `HashMap<String, Value>` (JSON values stored as strings).
    async fn read_state_hash(&self, key: &str) -> Result<HashMap<String, Value>> {
        let raw: HashMap<String, String> = self
            .client
            .hgetall(key)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hgetall failed: {e}")))?;
        let mut map = HashMap::new();
        for (k, v) in raw {
            let val: Value = serde_json::from_str(&v).unwrap_or(Value::String(v));
            map.insert(k, val);
        }
        Ok(map)
    }

    /// Write a `HashMap<String, Value>` into a Redis hash within a transaction.
    async fn write_state_hash(
        trx: &Transaction,
        key: &str,
        state: &HashMap<String, Value>,
    ) -> Result<()> {
        if state.is_empty() {
            return Ok(());
        }
        let mut fields: Vec<(String, String)> = Vec::with_capacity(state.len());
        for (k, v) in state {
            let serialized = serde_json::to_string(v).map_err(|e| {
                adk_core::AdkError::Session(format!("serialize state value failed: {e}"))
            })?;
            fields.push((k.clone(), serialized));
        }
        let _: () = trx
            .hset(key, fields)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hset failed: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl SessionService for RedisSessionService {
    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        // Read existing app/user state and merge deltas
        let mut app_state = self.read_state_hash(&app_state_key(&req.app_name)).await?;
        app_state.extend(app_delta);

        let mut user_state =
            self.read_state_hash(&user_state_key(&req.app_name, &req.user_id)).await?;
        user_state.extend(user_delta);

        let merged_state = state_utils::merge_states(&app_state, &user_state, &session_state);

        // Atomic write via MULTI/EXEC
        let trx = self.client.multi();

        // Session metadata hash
        let session_k = session_key(&req.app_name, &req.user_id, &session_id);
        let state_json = serde_json::to_string(&merged_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize state failed: {e}")))?;
        let session_fields: Vec<(String, String)> = vec![
            ("app_name".into(), req.app_name.clone()),
            ("user_id".into(), req.user_id.clone()),
            ("session_id".into(), session_id.clone()),
            ("state".into(), state_json),
            ("created_at".into(), now.to_rfc3339()),
            ("updated_at".into(), now.to_rfc3339()),
        ];
        let _: () = trx
            .hset(&session_k, session_fields)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hset failed: {e}")))?;

        // App state hash
        Self::write_state_hash(&trx, &app_state_key(&req.app_name), &app_state).await?;

        // User state hash
        Self::write_state_hash(&trx, &user_state_key(&req.app_name, &req.user_id), &user_state)
            .await?;

        // Add session ID to index set
        let idx_k = index_key(&req.app_name, &req.user_id);
        let _: () = trx
            .sadd(&idx_k, &session_id)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis sadd failed: {e}")))?;

        // Reverse lookup: session_id → app_name:user_id
        let lk = lookup_key(&session_id);
        let lookup_val = format!("{}:{}", req.app_name, req.user_id);
        let _: () = trx
            .set(&lk, lookup_val, None, None, false)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis set failed: {e}")))?;

        let _: () = trx
            .exec(true)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis transaction failed: {e}")))?;

        // Apply TTL outside transaction
        let events_k = events_key(&req.app_name, &req.user_id, &session_id);
        self.apply_ttl(&session_k, &events_k).await?;

        Ok(Box::new(RedisSession {
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
        let session_k = session_key(&req.app_name, &req.user_id, &req.session_id);

        let exists: bool = self
            .client
            .exists(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis exists failed: {e}")))?;
        if !exists {
            return Err(adk_core::AdkError::Session("session not found".into()));
        }

        let raw: HashMap<String, String> = self
            .client
            .hgetall(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hgetall failed: {e}")))?;

        let updated_at: DateTime<Utc> =
            raw.get("updated_at").and_then(|s| s.parse().ok()).unwrap_or_else(Utc::now);

        let session_state: HashMap<String, Value> =
            raw.get("state").and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();

        // Read events from sorted set (ordered by score = timestamp millis)
        let events_k = events_key(&req.app_name, &req.user_id, &req.session_id);
        let raw_events: Vec<(String, f64)> = self
            .client
            .zrange(&events_k, 0, -1, None, false, None, true)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis zrange failed: {e}")))?;

        let mut events: Vec<Event> = raw_events
            .into_iter()
            .filter_map(|(json, _score)| serde_json::from_str(&json).ok())
            .collect();

        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(RedisSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state: session_state,
            events,
            updated_at,
        }))
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let idx_k = index_key(&req.app_name, &req.user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis smembers failed: {e}")))?;

        let offset = req.offset.unwrap_or(0);
        let limit = req.limit.unwrap_or(usize::MAX);

        let mut sessions: Vec<Box<dyn Session>> = Vec::new();
        for sid in session_ids.into_iter().skip(offset).take(limit) {
            let session_k = session_key(&req.app_name, &req.user_id, &sid);
            let raw: HashMap<String, String> =
                self.client.hgetall(&session_k).await.map_err(|e| {
                    adk_core::AdkError::Session(format!("redis hgetall failed: {e}"))
                })?;

            if raw.is_empty() {
                continue; // Key expired or was deleted
            }

            let state: HashMap<String, Value> =
                raw.get("state").and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();

            let updated_at: DateTime<Utc> =
                raw.get("updated_at").and_then(|s| s.parse().ok()).unwrap_or_else(Utc::now);

            sessions.push(Box::new(RedisSession {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: sid,
                state,
                events: Vec::new(),
                updated_at,
            }));
        }

        Ok(sessions)
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id, session_id = %req.session_id))]
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let session_k = session_key(&req.app_name, &req.user_id, &req.session_id);
        let events_k = events_key(&req.app_name, &req.user_id, &req.session_id);
        let idx_k = index_key(&req.app_name, &req.user_id);
        let lk = lookup_key(&req.session_id);

        let trx = self.client.multi();
        let _: () = trx
            .del(vec![session_k, events_k, lk])
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis del failed: {e}")))?;
        let _: () = trx
            .srem(&idx_k, &req.session_id)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis srem failed: {e}")))?;
        let _: () = trx
            .exec(true)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis transaction failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(session_id = %session_id))]
    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        // Strip temp: keys
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        // Use reverse lookup to find app_name and user_id
        let lk = lookup_key(session_id);
        let lookup_val: Option<String> = self
            .client
            .get(&lk)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis get failed: {e}")))?;

        let lookup_val =
            lookup_val.ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

        // Parse "app_name:user_id" — split on first ':'
        let (app_name, user_id) = lookup_val
            .split_once(':')
            .ok_or_else(|| adk_core::AdkError::Session("corrupt session lookup entry".into()))?;

        let session_k = session_key(app_name, user_id, session_id);

        // Verify session hash exists
        let exists: bool = self
            .client
            .exists(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis exists failed: {e}")))?;
        if !exists {
            return Err(adk_core::AdkError::Session("session not found".into()));
        }

        // Read existing session state
        let raw: HashMap<String, String> = self
            .client
            .hgetall(&session_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hgetall failed: {e}")))?;

        let existing_state: HashMap<String, Value> =
            raw.get("state").and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
        let (_, _, mut session_state) = state_utils::extract_state_deltas(&existing_state);

        // Load current app/user state
        let app_state = self.read_state_hash(&app_state_key(app_name)).await?;
        let user_state = self.read_state_hash(&user_state_key(app_name, user_id)).await?;

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);

        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);

        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);

        // Serialize event for sorted set
        let event_json = serde_json::to_string(&event)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize failed: {e}")))?;
        let score = event.timestamp.timestamp_millis() as f64;

        // Atomic write
        let trx = self.client.multi();

        Self::write_state_hash(&trx, &app_state_key(app_name), &new_app_state).await?;
        Self::write_state_hash(&trx, &user_state_key(app_name, user_id), &new_user_state).await?;

        // Update session merged state and timestamp
        let merged_state_json = serde_json::to_string(&merged_state)
            .map_err(|e| adk_core::AdkError::Session(format!("serialize state failed: {e}")))?;
        let _: () = trx
            .hset(
                &session_k,
                vec![
                    ("state".to_string(), merged_state_json),
                    ("updated_at".to_string(), event.timestamp.to_rfc3339()),
                ],
            )
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis hset failed: {e}")))?;

        // Add event to sorted set
        let events_k = events_key(app_name, user_id, session_id);
        let _: () = trx
            .zadd(&events_k, None, None, false, false, (score, event_json))
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis zadd failed: {e}")))?;

        let _: () = trx
            .exec(true)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis transaction failed: {e}")))?;

        // Refresh TTL
        self.apply_ttl(&session_k, &events_k).await?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let idx_k = index_key(app_name, user_id);
        let session_ids: Vec<String> = self
            .client
            .smembers(&idx_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis smembers failed: {e}")))?;

        if session_ids.is_empty() {
            return Ok(());
        }

        let trx = self.client.multi();
        for sid in &session_ids {
            let sk = session_key(app_name, user_id, sid);
            let ek = events_key(app_name, user_id, sid);
            let lk = lookup_key(sid);
            let _: () = trx
                .del(vec![sk, ek, lk])
                .await
                .map_err(|e| adk_core::AdkError::Session(format!("redis del failed: {e}")))?;
        }
        let _: () = trx
            .del(&idx_k)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis del failed: {e}")))?;
        let _: () = trx
            .exec(true)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("redis transaction failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        let _: String = self
            .client
            .ping(None)
            .await
            .map_err(|e| adk_core::AdkError::Session(format!("health check failed: {e}")))?;
        Ok(())
    }
}

/// In-memory representation of a session retrieved from Redis.
struct RedisSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for RedisSession {
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

impl State for RedisSession {
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

impl Events for RedisSession {
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
