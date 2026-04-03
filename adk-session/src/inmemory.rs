use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_APP,
    KEY_PREFIX_TEMP, KEY_PREFIX_USER, ListRequest, Session, SessionService, State,
};
use adk_core::Result;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

type StateMap = HashMap<String, Value>;

#[derive(Clone)]
struct SessionData {
    identity: AdkIdentity,
    events: Vec<Event>,
    state: StateMap,
    updated_at: DateTime<Utc>,
}

pub struct InMemorySessionService {
    sessions: Arc<RwLock<HashMap<AdkIdentity, SessionData>>>,
    app_state: Arc<RwLock<HashMap<String, StateMap>>>,
    user_state: Arc<RwLock<HashMap<String, HashMap<String, StateMap>>>>,
}

impl InMemorySessionService {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            app_state: Arc::new(RwLock::new(HashMap::new())),
            user_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn extract_state_deltas(delta: &HashMap<String, Value>) -> (StateMap, StateMap, StateMap) {
        let mut app_delta = StateMap::new();
        let mut user_delta = StateMap::new();
        let mut session_delta = StateMap::new();

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

    fn merge_states(app: &StateMap, user: &StateMap, session: &StateMap) -> StateMap {
        let mut merged = session.clone();
        for (k, v) in app {
            merged.insert(format!("{KEY_PREFIX_APP}{k}"), v.clone());
        }
        for (k, v) in user {
            merged.insert(format!("{KEY_PREFIX_USER}{k}"), v.clone());
        }
        merged
    }

    /// Build an [`AdkIdentity`] from raw string fields, returning a session
    /// error if any field fails validation.
    fn make_identity(app_name: &str, user_id: &str, session_id: &str) -> Result<AdkIdentity> {
        Ok(AdkIdentity::new(
            AppName::try_from(app_name)
                .map_err(|e| adk_core::AdkError::session(format!("invalid app_name: {e}")))?,
            UserId::try_from(user_id)
                .map_err(|e| adk_core::AdkError::session(format!("invalid user_id: {e}")))?,
            SessionId::try_from(session_id)
                .map_err(|e| adk_core::AdkError::session(format!("invalid session_id: {e}")))?,
        ))
    }
}

impl Default for InMemorySessionService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionService for InMemorySessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id_str = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());

        let identity = Self::make_identity(&req.app_name, &req.user_id, &session_id_str)?;

        let (app_delta, user_delta, session_state) = Self::extract_state_deltas(&req.state);

        let mut app_state_lock = self.app_state.write().unwrap();
        let app_state = app_state_lock.entry(req.app_name.clone()).or_default();
        app_state.extend(app_delta);
        let app_state_clone = app_state.clone();
        drop(app_state_lock);

        let mut user_state_lock = self.user_state.write().unwrap();
        let user_map = user_state_lock.entry(req.app_name.clone()).or_default();
        let user_state = user_map.entry(req.user_id.clone()).or_default();
        user_state.extend(user_delta);
        let user_state_clone = user_state.clone();
        drop(user_state_lock);

        let merged_state = Self::merge_states(&app_state_clone, &user_state_clone, &session_state);

        let data = SessionData {
            identity: identity.clone(),
            events: Vec::new(),
            state: merged_state.clone(),
            updated_at: Utc::now(),
        };

        let mut sessions = self.sessions.write().unwrap();
        sessions.insert(identity.clone(), data);
        drop(sessions);

        Ok(Box::new(InMemorySession {
            identity,
            state: merged_state,
            events: Vec::new(),
            updated_at: Utc::now(),
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let identity = Self::make_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let sessions = self.sessions.read().unwrap();
        let data = sessions
            .get(&identity)
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_state_lock = self.app_state.read().unwrap();
        let app_state = app_state_lock.get(&req.app_name).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap();
        let user_state = user_state_lock
            .get(&req.app_name)
            .and_then(|m| m.get(&req.user_id))
            .cloned()
            .unwrap_or_default();
        drop(user_state_lock);

        let merged_state = Self::merge_states(&app_state, &user_state, &data.state);

        let mut events = data.events.clone();
        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(InMemorySession {
            identity: data.identity.clone(),
            state: merged_state,
            events,
            updated_at: data.updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let sessions = self.sessions.read().unwrap();
        let offset = req.offset.unwrap_or(0);
        let limit = req.limit.unwrap_or(usize::MAX);
        let mut result = Vec::new();

        for data in sessions.values() {
            if data.identity.app_name.as_ref() == req.app_name
                && data.identity.user_id.as_ref() == req.user_id
            {
                result.push(data.clone());
            }
        }

        // Sort by updated_at descending for consistency with other backends
        result.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

        let result: Vec<Box<dyn Session>> = result
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|data| {
                Box::new(InMemorySession {
                    identity: data.identity,
                    state: data.state,
                    events: data.events,
                    updated_at: data.updated_at,
                }) as Box<dyn Session>
            })
            .collect();

        Ok(result)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let identity = Self::make_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let mut sessions = self.sessions.write().unwrap();
        sessions.remove(&identity);
        Ok(())
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        sessions.retain(|_, data| {
            !(data.identity.app_name.as_ref() == app_name
                && data.identity.user_id.as_ref() == user_id)
        });
        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let (app_name, user_id, app_delta, user_delta, _session_delta) = {
            let mut sessions = self.sessions.write().unwrap();
            let data = sessions
                .values_mut()
                .find(|d| d.identity.session_id.as_ref() == session_id)
                .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

            data.events.push(event.clone());
            data.updated_at = event.timestamp;

            let (app_delta, user_delta, session_delta) =
                Self::extract_state_deltas(&event.actions.state_delta);
            data.state.extend(session_delta.clone());

            (
                data.identity.app_name.as_ref().to_string(),
                data.identity.user_id.as_ref().to_string(),
                app_delta,
                user_delta,
                session_delta,
            )
        };

        if !app_delta.is_empty() {
            let mut app_state_lock = self.app_state.write().unwrap();
            let app_state = app_state_lock.entry(app_name.clone()).or_default();
            app_state.extend(app_delta);
        }

        if !user_delta.is_empty() {
            let mut user_state_lock = self.user_state.write().unwrap();
            let user_map = user_state_lock.entry(app_name).or_default();
            let user_state = user_map.entry(user_id).or_default();
            user_state.extend(user_delta);
        }

        Ok(())
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let identity = req.identity;

        let (app_name_str, user_id_str, app_delta, user_delta) = {
            let mut sessions = self.sessions.write().unwrap();
            let data = sessions
                .get_mut(&identity)
                .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

            data.events.push(event.clone());
            data.updated_at = event.timestamp;

            let (app_delta, user_delta, session_delta) =
                Self::extract_state_deltas(&event.actions.state_delta);
            data.state.extend(session_delta);

            (
                identity.app_name.as_ref().to_string(),
                identity.user_id.as_ref().to_string(),
                app_delta,
                user_delta,
            )
        };

        if !app_delta.is_empty() {
            let mut app_state_lock = self.app_state.write().unwrap();
            let app_state = app_state_lock.entry(app_name_str.clone()).or_default();
            app_state.extend(app_delta);
        }

        if !user_delta.is_empty() {
            let mut user_state_lock = self.user_state.write().unwrap();
            let user_map = user_state_lock.entry(app_name_str).or_default();
            let user_state = user_map.entry(user_id_str).or_default();
            user_state.extend(user_delta);
        }

        Ok(())
    }

    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        let sessions = self.sessions.read().unwrap();
        let data = sessions
            .get(identity)
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_state_lock = self.app_state.read().unwrap();
        let app_state = app_state_lock.get(identity.app_name.as_ref()).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap();
        let user_state = user_state_lock
            .get(identity.app_name.as_ref())
            .and_then(|m| m.get(identity.user_id.as_ref()))
            .cloned()
            .unwrap_or_default();
        drop(user_state_lock);

        let merged_state = Self::merge_states(&app_state, &user_state, &data.state);

        Ok(Box::new(InMemorySession {
            identity: data.identity.clone(),
            state: merged_state,
            events: data.events.clone(),
            updated_at: data.updated_at,
        }))
    }

    async fn delete_for_identity(&self, identity: &AdkIdentity) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap();
        sessions.remove(identity);
        Ok(())
    }
}

struct InMemorySession {
    identity: AdkIdentity,
    state: StateMap,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for InMemorySession {
    fn id(&self) -> &str {
        self.identity.session_id.as_ref()
    }

    fn app_name(&self) -> &str {
        self.identity.app_name.as_ref()
    }

    fn user_id(&self) -> &str {
        self.identity.user_id.as_ref()
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

impl State for InMemorySession {
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

impl Events for InMemorySession {
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
