use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
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

/// In-memory session service for testing and lightweight deployments.
///
/// All data is stored in process memory and lost on restart.
pub struct InMemorySessionService {
    sessions: Arc<RwLock<HashMap<AdkIdentity, SessionData>>>,
    app_state: Arc<RwLock<HashMap<String, StateMap>>>,
    user_state: Arc<RwLock<HashMap<String, HashMap<String, StateMap>>>>,
}

impl InMemorySessionService {
    /// Creates a new empty in-memory session service.
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            app_state: Arc::new(RwLock::new(HashMap::new())),
            user_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn extract_state_deltas(delta: &HashMap<String, Value>) -> (StateMap, StateMap, StateMap) {
        state_utils::extract_state_deltas(delta)
    }

    fn merge_states(app: &StateMap, user: &StateMap, session: &StateMap) -> StateMap {
        state_utils::merge_states(app, user, session)
    }

    /// Builds an [`AdkIdentity`] from raw string fields.
    fn make_identity(app_name: &str, user_id: &str, session_id: &str) -> Result<AdkIdentity> {
        Ok(AdkIdentity::new(
            AppName::try_from(app_name)?,
            UserId::try_from(user_id)?,
            SessionId::try_from(session_id)?,
        ))
    }

    /// Rewind a session to before all events (remove all events and reset state).
    async fn rewind_to_empty(&self, session_id: &str) -> Result<Box<dyn Session>> {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        let data = sessions
            .values_mut()
            .find(|d| d.identity.session_id.as_ref() == session_id)
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        data.events.clear();
        data.state = HashMap::new();
        data.updated_at = Utc::now();

        let app_name = data.identity.app_name.as_ref().to_string();
        let user_id = data.identity.user_id.as_ref().to_string();
        let identity = data.identity.clone();
        let updated_at = data.updated_at;
        drop(sessions);

        let app_state_lock = self.app_state.read().unwrap_or_else(|e| e.into_inner());
        let app_state = app_state_lock.get(&app_name).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap_or_else(|e| e.into_inner());
        let user_state = user_state_lock
            .get(&app_name)
            .and_then(|m| m.get(&user_id))
            .cloned()
            .unwrap_or_default();
        drop(user_state_lock);

        let merged_state = state_utils::merge_states(&app_state, &user_state, &HashMap::new());

        Ok(Box::new(InMemorySession {
            identity,
            state: merged_state,
            events: Vec::new(),
            updated_at,
        }))
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

        let mut app_state_lock = self.app_state.write().unwrap_or_else(|e| e.into_inner());
        let app_state = app_state_lock.entry(req.app_name.clone()).or_default();
        app_state.extend(app_delta);
        let app_state_clone = app_state.clone();
        drop(app_state_lock);

        let mut user_state_lock = self.user_state.write().unwrap_or_else(|e| e.into_inner());
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

        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
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

        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        let data =
            sessions.get(&identity).ok_or_else(|| crate::service::session_not_found(&req))?;

        let app_state_lock = self.app_state.read().unwrap_or_else(|e| e.into_inner());
        let app_state = app_state_lock.get(&req.app_name).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap_or_else(|e| e.into_inner());
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
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
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

        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.remove(&identity);
        Ok(())
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, data| {
            !(data.identity.app_name.as_ref() == app_name
                && data.identity.user_id.as_ref() == user_id)
        });
        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let (app_name, user_id, app_delta, user_delta, _session_delta) = {
            let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
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
            let mut app_state_lock = self.app_state.write().unwrap_or_else(|e| e.into_inner());
            let app_state = app_state_lock.entry(app_name.clone()).or_default();
            app_state.extend(app_delta);
        }

        if !user_delta.is_empty() {
            let mut user_state_lock = self.user_state.write().unwrap_or_else(|e| e.into_inner());
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
            let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
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
            let mut app_state_lock = self.app_state.write().unwrap_or_else(|e| e.into_inner());
            let app_state = app_state_lock.entry(app_name_str.clone()).or_default();
            app_state.extend(app_delta);
        }

        if !user_delta.is_empty() {
            let mut user_state_lock = self.user_state.write().unwrap_or_else(|e| e.into_inner());
            let user_map = user_state_lock.entry(app_name_str).or_default();
            let user_state = user_map.entry(user_id_str).or_default();
            user_state.extend(user_delta);
        }

        Ok(())
    }

    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
        let data = sessions
            .get(identity)
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_state_lock = self.app_state.read().unwrap_or_else(|e| e.into_inner());
        let app_state = app_state_lock.get(identity.app_name.as_ref()).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap_or_else(|e| e.into_inner());
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
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.remove(identity);
        Ok(())
    }

    async fn rewind(&self, session_id: &str, target_event_id: &str) -> Result<Box<dyn Session>> {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());

        // Find the session by session_id
        let data = sessions
            .values_mut()
            .find(|d| d.identity.session_id.as_ref() == session_id)
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        // Find the target event index
        let target_index =
            data.events.iter().position(|e| e.id == target_event_id).ok_or_else(|| {
                adk_core::AdkError::session(format!("target event not found: {target_event_id}"))
            })?;

        // Truncate events after the target (keep 0..=target_index)
        data.events.truncate(target_index + 1);

        // Rebuild session state from remaining events' state deltas
        let mut rebuilt_session_state: HashMap<String, Value> = HashMap::new();
        for event in &data.events {
            let (_app_delta, _user_delta, session_delta) =
                state_utils::extract_state_deltas(&event.actions.state_delta);
            rebuilt_session_state.extend(session_delta);
        }

        // Get app and user state (these are not rewound — they are separate)
        let app_name = data.identity.app_name.as_ref().to_string();
        let user_id = data.identity.user_id.as_ref().to_string();

        // Update the stored session state with rebuilt session-level state
        data.state = rebuilt_session_state.clone();
        data.updated_at = data.events.last().map(|e| e.timestamp).unwrap_or(Utc::now());

        let identity = data.identity.clone();
        let events = data.events.clone();
        let updated_at = data.updated_at;
        drop(sessions);

        // Merge with app and user state for the returned session
        let app_state_lock = self.app_state.read().unwrap_or_else(|e| e.into_inner());
        let app_state = app_state_lock.get(&app_name).cloned().unwrap_or_default();
        drop(app_state_lock);

        let user_state_lock = self.user_state.read().unwrap_or_else(|e| e.into_inner());
        let user_state = user_state_lock
            .get(&app_name)
            .and_then(|m| m.get(&user_id))
            .cloned()
            .unwrap_or_default();
        drop(user_state_lock);

        let merged_state =
            state_utils::merge_states(&app_state, &user_state, &rebuilt_session_state);

        Ok(Box::new(InMemorySession { identity, state: merged_state, events, updated_at }))
    }

    async fn rewind_steps(&self, session_id: &str, steps: usize) -> Result<Box<dyn Session>> {
        if steps == 0 {
            // Return the session unchanged
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            let data = sessions
                .values()
                .find(|d| d.identity.session_id.as_ref() == session_id)
                .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

            let app_name = data.identity.app_name.as_ref().to_string();
            let user_id = data.identity.user_id.as_ref().to_string();
            let identity = data.identity.clone();
            let events = data.events.clone();
            let session_state = data.state.clone();
            let updated_at = data.updated_at;
            drop(sessions);

            let app_state_lock = self.app_state.read().unwrap_or_else(|e| e.into_inner());
            let app_state = app_state_lock.get(&app_name).cloned().unwrap_or_default();
            drop(app_state_lock);

            let user_state_lock = self.user_state.read().unwrap_or_else(|e| e.into_inner());
            let user_state = user_state_lock
                .get(&app_name)
                .and_then(|m| m.get(&user_id))
                .cloned()
                .unwrap_or_default();
            drop(user_state_lock);

            let merged_state = state_utils::merge_states(&app_state, &user_state, &session_state);

            return Ok(Box::new(InMemorySession {
                identity,
                state: merged_state,
                events,
                updated_at,
            }));
        }

        // Read the event count and determine target
        let rewind_target = {
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            let data = sessions
                .values()
                .find(|d| d.identity.session_id.as_ref() == session_id)
                .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

            if steps > data.events.len() {
                return Err(adk_core::AdkError::session("rewind steps exceeds event count"));
            }

            let target_index = data.events.len() - steps;
            if target_index == 0 {
                // Rewinding all events
                None
            } else {
                Some(data.events[target_index - 1].id.clone())
            }
        };

        match rewind_target {
            Some(target_event_id) => self.rewind(session_id, &target_event_id).await,
            None => self.rewind_to_empty(session_id).await,
        }
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
