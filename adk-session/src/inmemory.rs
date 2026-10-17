use crate::service::{
    ambiguous_session_id, identity_not_found, rewind_event_not_found, rewind_steps_exceed,
    session_already_exists, session_id_not_found, validate_scope, validate_session_id,
};
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
    /// The session tier.
    state: StateMap,
    /// The session tier the session was created with; rewind rebuilds from it.
    initial_state: StateMap,
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

    /// Builds an [`AdkIdentity`] from raw string fields.
    fn make_identity(app_name: &str, user_id: &str, session_id: &str) -> Result<AdkIdentity> {
        Ok(AdkIdentity::new(
            AppName::try_from(app_name)?,
            UserId::try_from(user_id)?,
            SessionId::try_from(session_id)?,
        ))
    }

    /// The current app and user tiers.
    fn tiers(&self, app_name: &str, user_id: &str) -> (StateMap, StateMap) {
        let app_state = self
            .app_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(app_name)
            .cloned()
            .unwrap_or_default();
        let user_state = self
            .user_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(app_name)
            .and_then(|users| users.get(user_id))
            .cloned()
            .unwrap_or_default();
        (app_state, user_state)
    }

    /// The session as callers see it: `session_state` merged with the current app and user tiers.
    fn view(
        &self,
        identity: AdkIdentity,
        session_state: &StateMap,
        events: Vec<Event>,
        updated_at: DateTime<Utc>,
    ) -> Box<dyn Session> {
        let (app_state, user_state) =
            self.tiers(identity.app_name.as_ref(), identity.user_id.as_ref());
        Box::new(InMemorySession {
            state: state_utils::merge_states(&app_state, &user_state, session_state),
            identity,
            events,
            updated_at,
        })
    }

    /// Appends `event` to the session `identity`, which must exist.
    fn append_to(&self, identity: &AdkIdentity, mut event: Event) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));
        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        {
            let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            let data = sessions.get_mut(identity).ok_or_else(|| identity_not_found(identity))?;
            data.updated_at = event.timestamp;
            data.state.extend(session_delta);
            data.events.push(event);
        }

        let app_name = identity.app_name.as_ref();
        if !app_delta.is_empty() {
            let mut app_state = self.app_state.write().unwrap_or_else(|e| e.into_inner());
            app_state.entry(app_name.to_string()).or_default().extend(app_delta);
        }
        if !user_delta.is_empty() {
            let mut user_state = self.user_state.write().unwrap_or_else(|e| e.into_inner());
            user_state
                .entry(app_name.to_string())
                .or_default()
                .entry(identity.user_id.as_ref().to_string())
                .or_default()
                .extend(user_delta);
        }
        Ok(())
    }

    /// Keeps the first `keep(events)` events of `identity` and rebuilds its session tier from
    /// the state it was created with and their deltas, under one write lock.
    fn truncate(
        &self,
        identity: &AdkIdentity,
        keep: impl FnOnce(&[Event]) -> Result<usize>,
    ) -> Result<Box<dyn Session>> {
        let (events, state, updated_at) = {
            let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            let data = sessions.get_mut(identity).ok_or_else(|| identity_not_found(identity))?;
            let keep = keep(&data.events)?;
            data.events.truncate(keep);
            data.state = state_utils::rebuild_session_state(&data.initial_state, &data.events);
            data.updated_at = Utc::now();
            (data.events.clone(), data.state.clone(), data.updated_at)
        };
        Ok(self.view(identity.clone(), &state, events, updated_at))
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
        let identity = Self::make_identity(&req.app_name, &req.user_id, &session_id)?;
        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);
        let now = Utc::now();

        // Held until the session is inserted, so a concurrent create of the same identity sees
        // it; `get` takes the session lock before the tier locks too.
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        if sessions.contains_key(&identity) {
            return Err(session_already_exists(&req.app_name, &req.user_id, &session_id));
        }

        let app_state = {
            let mut app_state = self.app_state.write().unwrap_or_else(|e| e.into_inner());
            let tier = app_state.entry(req.app_name.clone()).or_default();
            tier.extend(app_delta);
            tier.clone()
        };
        let user_state = {
            let mut user_state = self.user_state.write().unwrap_or_else(|e| e.into_inner());
            let tier = user_state
                .entry(req.app_name.clone())
                .or_default()
                .entry(req.user_id.clone())
                .or_default();
            tier.extend(user_delta);
            tier.clone()
        };

        sessions.insert(
            identity.clone(),
            SessionData {
                identity: identity.clone(),
                events: Vec::new(),
                state: session_state.clone(),
                initial_state: session_state.clone(),
                updated_at: now,
            },
        );
        drop(sessions);

        Ok(Box::new(InMemorySession {
            identity,
            state: state_utils::merge_states(&app_state, &user_state, &session_state),
            events: Vec::new(),
            updated_at: now,
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let identity = Self::make_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let (mut events, state, updated_at) = {
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            let data =
                sessions.get(&identity).ok_or_else(|| crate::service::session_not_found(&req))?;
            (data.events.clone(), data.state.clone(), data.updated_at)
        };
        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events.drain(..start);
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(self.view(identity, &state, events, updated_at))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        validate_scope(&req.app_name, &req.user_id)?;
        let mut result: Vec<SessionData> = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|data| {
                data.identity.app_name.as_ref() == req.app_name
                    && data.identity.user_id.as_ref() == req.user_id
            })
            .cloned()
            .collect();

        // Newest first, ties by session ID descending, as every backend orders them.
        result.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.identity.session_id.as_ref().cmp(a.identity.session_id.as_ref()))
        });

        let (app_state, user_state) = self.tiers(&req.app_name, &req.user_id);

        Ok(result
            .into_iter()
            .skip(req.offset.unwrap_or(0))
            .take(req.limit.unwrap_or(usize::MAX))
            .map(|data| {
                Box::new(InMemorySession {
                    identity: data.identity,
                    state: state_utils::merge_current_tiers(&app_state, &user_state, &data.state),
                    events: data.events,
                    updated_at: data.updated_at,
                }) as Box<dyn Session>
            })
            .collect())
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        let identity = Self::make_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.remove(&identity);
        Ok(())
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, data| {
            !(data.identity.app_name.as_ref() == app_name
                && data.identity.user_id.as_ref() == user_id)
        });
        Ok(())
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        validate_session_id(session_id)?;
        let owners: Vec<AdkIdentity> = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|identity| identity.session_id.as_ref() == session_id)
            .cloned()
            .collect();
        match owners.as_slice() {
            [] => Err(session_id_not_found(session_id)),
            [identity] => self.append_to(identity, event),
            _ => Err(ambiguous_session_id(session_id)),
        }
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        self.append_to(&req.identity, req.event)
    }

    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        let (events, state, updated_at) = {
            let sessions = self.sessions.read().unwrap_or_else(|e| e.into_inner());
            let data = sessions.get(identity).ok_or_else(|| identity_not_found(identity))?;
            (data.events.clone(), data.state.clone(), data.updated_at)
        };
        Ok(self.view(identity.clone(), &state, events, updated_at))
    }

    async fn delete_for_identity(&self, identity: &AdkIdentity) -> Result<()> {
        let mut sessions = self.sessions.write().unwrap_or_else(|e| e.into_inner());
        sessions.remove(identity);
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
