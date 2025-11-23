use crate::{
    CreateRequest, DeleteRequest, Event, Events, GetRequest, ListRequest, Session,
    SessionService, State, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

type StateMap = HashMap<String, Value>;

#[derive(Clone)]
struct SessionData {
    id: SessionId,
    events: Vec<Event>,
    state: StateMap,
    updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionId {
    app_name: String,
    user_id: String,
    session_id: String,
}

impl SessionId {
    fn key(&self) -> String {
        format!("{}:{}:{}", self.app_name, self.user_id, self.session_id)
    }
}

pub struct InMemorySessionService {
    sessions: Arc<RwLock<HashMap<String, SessionData>>>,
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
            merged.insert(format!("{}{}", KEY_PREFIX_APP, k), v.clone());
        }
        for (k, v) in user {
            merged.insert(format!("{}{}", KEY_PREFIX_USER, k), v.clone());
        }
        merged
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
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        
        let id = SessionId {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: session_id.clone(),
        };

        let (app_delta, user_delta, session_state) = Self::extract_state_deltas(&req.state);

        let mut app_state_lock = self.app_state.write().unwrap();
        let app_state = app_state_lock.entry(req.app_name.clone()).or_insert_with(HashMap::new);
        app_state.extend(app_delta);
        let app_state_clone = app_state.clone();
        drop(app_state_lock);

        let mut user_state_lock = self.user_state.write().unwrap();
        let user_map = user_state_lock.entry(req.app_name.clone()).or_insert_with(HashMap::new);
        let user_state = user_map.entry(req.user_id.clone()).or_insert_with(HashMap::new);
        user_state.extend(user_delta);
        let user_state_clone = user_state.clone();
        drop(user_state_lock);

        let merged_state = Self::merge_states(&app_state_clone, &user_state_clone, &session_state);

        let data = SessionData {
            id: id.clone(),
            events: Vec::new(),
            state: merged_state.clone(),
            updated_at: Utc::now(),
        };

        let mut sessions = self.sessions.write().unwrap();
        sessions.insert(id.key(), data);
        drop(sessions);

        Ok(Box::new(InMemorySession {
            id,
            state: merged_state,
            events: Vec::new(),
            updated_at: Utc::now(),
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let id = SessionId {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: req.session_id.clone(),
        };

        let sessions = self.sessions.read().unwrap();
        let data = sessions.get(&id.key())
            .ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

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
            id: data.id.clone(),
            state: merged_state,
            events,
            updated_at: data.updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let sessions = self.sessions.read().unwrap();
        let mut result = Vec::new();

        for data in sessions.values() {
            if data.id.app_name == req.app_name && data.id.user_id == req.user_id {
                result.push(Box::new(InMemorySession {
                    id: data.id.clone(),
                    state: data.state.clone(),
                    events: data.events.clone(),
                    updated_at: data.updated_at,
                }) as Box<dyn Session>);
            }
        }

        Ok(result)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let id = SessionId {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
        };

        let mut sessions = self.sessions.write().unwrap();
        sessions.remove(&id.key());
        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let (app_name, user_id, app_delta, user_delta, _session_delta) = {
            let mut sessions = self.sessions.write().unwrap();
            let data = sessions.values_mut()
                .find(|d| d.id.session_id == session_id)
                .ok_or_else(|| adk_core::AdkError::Session("session not found".into()))?;

            data.events.push(event.clone());
            data.updated_at = event.timestamp;

            let (app_delta, user_delta, session_delta) = Self::extract_state_deltas(&event.actions.state_delta);
            data.state.extend(session_delta.clone());

            (data.id.app_name.clone(), data.id.user_id.clone(), app_delta, user_delta, session_delta)
        };

        if !app_delta.is_empty() {
            let mut app_state_lock = self.app_state.write().unwrap();
            let app_state = app_state_lock.entry(app_name.clone()).or_insert_with(HashMap::new);
            app_state.extend(app_delta);
        }

        if !user_delta.is_empty() {
            let mut user_state_lock = self.user_state.write().unwrap();
            let user_map = user_state_lock.entry(app_name).or_insert_with(HashMap::new);
            let user_state = user_map.entry(user_id).or_insert_with(HashMap::new);
            user_state.extend(user_delta);
        }

        Ok(())
    }
}

struct InMemorySession {
    id: SessionId,
    state: StateMap,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for InMemorySession {
    fn id(&self) -> &str {
        &self.id.session_id
    }

    fn app_name(&self) -> &str {
        &self.id.app_name
    }

    fn user_id(&self) -> &str {
        &self.id.user_id
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
