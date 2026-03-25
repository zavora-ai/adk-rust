//! Firestore session service backend.
//!
//! Provides [`FirestoreSessionService`] for session persistence using Google Cloud Firestore.
//! Enabled via the `firestore` feature flag.
//!
//! # Data Organization
//!
//! Firestore uses subcollections to organize session data:
//!
//! - Session: `{root}/{app_name}/sessions/{session_id}`
//! - Event: `{root}/{app_name}/sessions/{session_id}/events/{event_id}`
//! - App state: `{root}/{app_name}/app_state`
//! - User state: `{root}/{app_name}/users/{user_id}/state`

use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firestore::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

const DEFAULT_ROOT_COLLECTION: &str = "adk_sessions";

/// Configuration for connecting to Firestore.
///
/// # Example
///
/// ```rust,ignore
/// use adk_session::FirestoreSessionConfig;
///
/// let config = FirestoreSessionConfig {
///     project_id: "my-gcp-project".to_string(),
///     root_collection: None, // defaults to "adk_sessions"
/// };
/// ```
pub struct FirestoreSessionConfig {
    /// Google Cloud project ID.
    pub project_id: String,
    /// Root collection prefix for namespacing session data.
    /// Defaults to `"adk_sessions"` when `None`.
    pub root_collection: Option<String>,
}

/// Firestore-backed session service implementing [`SessionService`](crate::SessionService).
///
/// Uses Google Cloud Firestore with Application Default Credentials for authentication.
/// Data is organized using subcollections under a configurable root collection.
pub struct FirestoreSessionService {
    db: FirestoreDb,
    root_collection: String,
}

impl FirestoreSessionService {
    /// Connect to Firestore using Application Default Credentials.
    ///
    /// Returns an error with "firestore connection failed" context if the connection cannot
    /// be established.
    ///
    /// # Arguments
    ///
    /// * `config` - Firestore connection configuration including project ID and optional
    ///   root collection prefix.
    pub async fn new(config: FirestoreSessionConfig) -> Result<Self> {
        let root_collection =
            config.root_collection.unwrap_or_else(|| DEFAULT_ROOT_COLLECTION.to_string());
        let db = FirestoreDb::new(&config.project_id).await.map_err(|e| {
            adk_core::AdkError::session(format!("firestore connection failed: {e}"))
        })?;
        Ok(Self { db, root_collection })
    }

    /// Returns a reference to the underlying `FirestoreDb`.
    pub fn db(&self) -> &FirestoreDb {
        &self.db
    }

    /// Returns the root collection prefix.
    pub fn root_collection(&self) -> &str {
        &self.root_collection
    }
}

/// Generate the Firestore document path for a session.
///
/// Path format: `{root}/{app_name}/sessions/{session_id}`
pub fn session_path(root: &str, app_name: &str, session_id: &str) -> String {
    format!("{root}/{app_name}/sessions/{session_id}")
}

/// Generate the Firestore document path for an event within a session.
///
/// Path format: `{root}/{app_name}/sessions/{session_id}/events/{event_id}`
pub fn event_path(root: &str, app_name: &str, session_id: &str, event_id: &str) -> String {
    format!("{root}/{app_name}/sessions/{session_id}/events/{event_id}")
}

/// Generate the Firestore document path for app-level state.
///
/// Path format: `{root}/{app_name}/app_state`
pub fn app_state_path(root: &str, app_name: &str) -> String {
    format!("{root}/{app_name}/app_state")
}

/// Generate the Firestore document path for user-level state.
///
/// Path format: `{root}/{app_name}/users/{user_id}/state`
pub fn user_state_path(root: &str, app_name: &str, user_id: &str) -> String {
    format!("{root}/{app_name}/users/{user_id}/state")
}

// ---------------------------------------------------------------------------
// Firestore document models
// ---------------------------------------------------------------------------

/// Firestore document for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionDoc {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    created_at: DateTime<Utc>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: DateTime<Utc>,
}

/// Firestore document for an event.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EventDoc {
    id: String,
    invocation_id: String,
    branch: String,
    author: String,
    #[serde(with = "firestore::serialize_as_timestamp")]
    timestamp: DateTime<Utc>,
    llm_response: Value,
    actions: Value,
    long_running_tool_ids: Value,
}

/// Firestore document for app-level state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppStateDoc {
    state: HashMap<String, Value>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: DateTime<Utc>,
}

/// Firestore document for user-level state.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserStateDoc {
    state: HashMap<String, Value>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Helper: convert Event <-> EventDoc
// ---------------------------------------------------------------------------

fn event_to_doc(event: &Event) -> std::result::Result<EventDoc, adk_core::AdkError> {
    let llm_response = serde_json::to_value(&event.llm_response)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let actions = serde_json::to_value(&event.actions)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;
    let long_running_tool_ids = serde_json::to_value(&event.long_running_tool_ids)
        .map_err(|e| adk_core::AdkError::session(format!("serialize failed: {e}")))?;

    Ok(EventDoc {
        id: event.id.clone(),
        invocation_id: event.invocation_id.clone(),
        branch: event.branch.clone(),
        author: event.author.clone(),
        timestamp: event.timestamp,
        llm_response,
        actions,
        long_running_tool_ids,
    })
}

fn doc_to_event(doc: &EventDoc) -> std::result::Result<Event, adk_core::AdkError> {
    let llm_response = serde_json::from_value(doc.llm_response.clone())
        .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}")))?;
    let actions = serde_json::from_value(doc.actions.clone())
        .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}")))?;
    let long_running_tool_ids = serde_json::from_value(doc.long_running_tool_ids.clone())
        .map_err(|e| adk_core::AdkError::session(format!("deserialize failed: {e}")))?;

    Ok(Event {
        id: doc.id.clone(),
        timestamp: doc.timestamp,
        invocation_id: doc.invocation_id.clone(),
        branch: doc.branch.clone(),
        author: doc.author.clone(),
        llm_request: None,
        llm_response,
        actions,
        long_running_tool_ids,
        provider_metadata: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Firestore collection helpers
//
// The Firestore fluent API uses (parent, collection_id, document_id) triples.
// For our subcollection layout:
//   sessions live at:  {root}/{app_name}/sessions/{session_id}
//     parent = db.parent_path(root, app_name)  →  .../root/app_name
//     collection = "sessions"
//     document_id = session_id
//
//   events live at:    {root}/{app_name}/sessions/{session_id}/events/{event_id}
//     parent = db.parent_path(root, app_name)?.at("sessions", session_id)?
//     collection = "events"
//     document_id = event_id
//
//   app_state lives at: {root}/{app_name}  (document "app_state" in root collection)
//     parent = <documents_path>
//     collection = root
//     document_id = app_name  → then sub-doc "app_state"
//
// Because Firestore's subcollection model is complex, we use the lower-level
// fluent API with explicit `parent()` calls.
// ---------------------------------------------------------------------------

impl FirestoreSessionService {
    /// Build the parent path for the sessions collection under a given app.
    /// Returns the parent string for `db.fluent().*.parent(parent)`.
    fn sessions_parent(&self, app_name: &str) -> std::result::Result<String, adk_core::AdkError> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| adk_core::AdkError::session(format!("path error: {e}")))?;
        Ok(parent.to_string())
    }

    /// Build the parent path for the events subcollection under a given session.
    fn events_parent(
        &self,
        app_name: &str,
        session_id: &str,
    ) -> std::result::Result<String, adk_core::AdkError> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| adk_core::AdkError::session(format!("path error: {e}")))?
            .at("sessions", session_id)
            .map_err(|e| adk_core::AdkError::session(format!("path error: {e}")))?;
        Ok(parent.to_string())
    }

    /// Build the parent path for the user state document.
    fn user_state_parent(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> std::result::Result<String, adk_core::AdkError> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| adk_core::AdkError::session(format!("path error: {e}")))?
            .at("users", user_id)
            .map_err(|e| adk_core::AdkError::session(format!("path error: {e}")))?;
        Ok(parent.to_string())
    }

    /// Read app-level state from Firestore.
    async fn read_app_state(
        &self,
        app_name: &str,
    ) -> std::result::Result<HashMap<String, Value>, adk_core::AdkError> {
        let doc: Option<AppStateDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(&self.root_collection)
            .obj::<AppStateDoc>()
            .one(format!("{app_name}/app_state"))
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        Ok(doc.map(|d| d.state).unwrap_or_default())
    }

    /// Read user-level state from Firestore.
    async fn read_user_state(
        &self,
        app_name: &str,
        user_id: &str,
    ) -> std::result::Result<HashMap<String, Value>, adk_core::AdkError> {
        let parent = self.user_state_parent(app_name, user_id)?;
        let doc: Option<UserStateDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in("state")
            .parent(&parent)
            .obj::<UserStateDoc>()
            .one("current")
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;
        Ok(doc.map(|d| d.state).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// SessionService implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionService for FirestoreSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now();

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        // Read existing app and user state, then merge deltas
        let existing_app_state = self.read_app_state(&req.app_name).await?;
        let mut new_app_state = existing_app_state;
        new_app_state.extend(app_delta);

        let existing_user_state = self.read_user_state(&req.app_name, &req.user_id).await?;
        let mut new_user_state = existing_user_state;
        new_user_state.extend(user_delta);

        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);

        // Persist atomically using a Firestore transaction
        let session_doc = SessionDoc {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: session_id.clone(),
            state: merged_state.clone(),
            created_at: now,
            updated_at: now,
        };
        let app_state_doc = AppStateDoc { state: new_app_state, updated_at: now };
        let user_state_doc = UserStateDoc { state: new_user_state, updated_at: now };

        let sessions_parent = self.sessions_parent(&req.app_name)?;
        let user_state_parent = self.user_state_parent(&req.app_name, &req.user_id)?;
        let root_col = self.root_collection.clone();
        let app_name_clone = req.app_name.clone();

        let mut transaction = self
            .db
            .begin_transaction()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // Upsert app state
        self.db
            .fluent()
            .update()
            .in_col(&root_col)
            .document_id(format!("{app_name_clone}/app_state"))
            .object(&app_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        // Upsert user state
        self.db
            .fluent()
            .update()
            .in_col("state")
            .document_id("current")
            .parent(&user_state_parent)
            .object(&user_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        // Create session document
        self.db
            .fluent()
            .update()
            .in_col("sessions")
            .document_id(&session_id)
            .parent(&sessions_parent)
            .object(&session_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("create failed: {e}")))?;

        transaction
            .commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(Box::new(FirestoreSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state: merged_state,
            events: Vec::new(),
            updated_at: now,
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        let sessions_parent = self.sessions_parent(&req.app_name)?;

        // Read session document
        let session_doc: SessionDoc = self
            .db
            .fluent()
            .select()
            .by_id_in("sessions")
            .parent(&sessions_parent)
            .obj::<SessionDoc>()
            .one(&req.session_id)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        // Read events subcollection ordered by timestamp
        let events_parent = self.events_parent(&req.app_name, &req.session_id)?;
        let event_docs: Vec<EventDoc> = self
            .db
            .fluent()
            .select()
            .from("events")
            .parent(&events_parent)
            .order_by([("timestamp".to_string(), FirestoreQueryDirection::Ascending)])
            .obj::<EventDoc>()
            .query()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let mut events: Vec<Event> =
            event_docs.iter().filter_map(|d| doc_to_event(d).ok()).collect();

        // Apply filters
        if let Some(num) = req.num_recent_events {
            let start = events.len().saturating_sub(num);
            events = events[start..].to_vec();
        }
        if let Some(after) = req.after {
            events.retain(|e| e.timestamp >= after);
        }

        Ok(Box::new(FirestoreSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state: session_doc.state,
            events,
            updated_at: session_doc.updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        let sessions_parent = self.sessions_parent(&req.app_name)?;

        let session_docs: Vec<SessionDoc> = self
            .db
            .fluent()
            .select()
            .from("sessions")
            .parent(&sessions_parent)
            .filter(|q| q.for_all([q.field("user_id").eq(&req.user_id)]))
            .obj::<SessionDoc>()
            .query()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let offset = req.offset.unwrap_or(0);
        let sessions: Vec<Box<dyn Session>> = session_docs
            .into_iter()
            .skip(offset)
            .take(req.limit.unwrap_or(usize::MAX))
            .map(|doc| {
                Box::new(FirestoreSession {
                    app_name: req.app_name.clone(),
                    user_id: req.user_id.clone(),
                    session_id: doc.session_id,
                    state: doc.state,
                    events: Vec::new(),
                    updated_at: doc.updated_at,
                }) as Box<dyn Session>
            })
            .collect();

        Ok(sessions)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        // First, list all event IDs in the subcollection so we can delete them
        let events_parent = self.events_parent(&req.app_name, &req.session_id)?;
        let event_docs: Vec<EventDoc> = self
            .db
            .fluent()
            .select()
            .from("events")
            .parent(&events_parent)
            .obj::<EventDoc>()
            .query()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;

        let sessions_parent = self.sessions_parent(&req.app_name)?;

        let mut transaction = self
            .db
            .begin_transaction()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // Delete all event documents
        for event_doc in &event_docs {
            self.db
                .fluent()
                .delete()
                .from("events")
                .parent(&events_parent)
                .document_id(&event_doc.id)
                .add_to_transaction(&mut transaction)
                .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;
        }

        // Delete session document
        self.db
            .fluent()
            .delete()
            .from("sessions")
            .parent(&sessions_parent)
            .document_id(&req.session_id)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("delete failed: {e}")))?;

        transaction
            .commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        // Strip temp: keys from state delta
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        // We need to find the session first to get app_name and user_id.
        // Firestore doesn't support cross-collection queries easily, so we use
        // a collection group query on "sessions" to find by session_id.
        let session_docs: Vec<SessionDoc> = self
            .db
            .fluent()
            .select()
            .from("sessions")
            .parent(self.db.get_documents_path())
            .all_descendants()
            .filter(|q| q.for_all([q.field("session_id").eq(session_id)]))
            .obj::<SessionDoc>()
            .query()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?;

        let session_doc =
            session_docs.first().ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let app_name = &session_doc.app_name;
        let user_id = &session_doc.user_id;
        let existing_state = &session_doc.state;

        let (_, _, mut session_state) = state_utils::extract_state_deltas(existing_state);

        // Load current app and user state
        let app_state = self.read_app_state(app_name).await?;
        let user_state = self.read_user_state(app_name, user_id).await?;

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let now = event.timestamp;

        // Merge deltas
        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);

        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);

        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);

        // Build updated documents
        let updated_session = SessionDoc {
            app_name: app_name.clone(),
            user_id: user_id.clone(),
            session_id: session_id.to_string(),
            state: merged_state,
            created_at: session_doc.created_at,
            updated_at: now,
        };
        let app_state_doc = AppStateDoc { state: new_app_state, updated_at: now };
        let user_state_doc = UserStateDoc { state: new_user_state, updated_at: now };
        let event_doc = event_to_doc(&event)?;

        let sessions_parent = self.sessions_parent(app_name)?;
        let events_parent = self.events_parent(app_name, session_id)?;
        let user_state_parent = self.user_state_parent(app_name, user_id)?;
        let root_col = self.root_collection.clone();

        let mut transaction = self
            .db
            .begin_transaction()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // Update app state
        self.db
            .fluent()
            .update()
            .in_col(&root_col)
            .document_id(format!("{app_name}/app_state"))
            .object(&app_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Update user state
        self.db
            .fluent()
            .update()
            .in_col("state")
            .document_id("current")
            .parent(&user_state_parent)
            .object(&user_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Update session document
        self.db
            .fluent()
            .update()
            .in_col("sessions")
            .document_id(session_id)
            .parent(&sessions_parent)
            .object(&updated_session)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Insert event document
        self.db
            .fluent()
            .update()
            .in_col("events")
            .document_id(&event.id)
            .parent(&events_parent)
            .object(&event_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        transaction
            .commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));

        let app_name = req.identity.app_name.as_ref();
        let user_id = req.identity.user_id.as_ref();
        let session_id = req.identity.session_id.as_ref();

        // Use the identity to read the session directly by document path — no
        // collection group query needed, so no ambiguity possible.
        let sessions_parent = self.sessions_parent(app_name)?;
        let session_doc: SessionDoc = self
            .db
            .fluent()
            .select()
            .by_id_in("sessions")
            .parent(&sessions_parent)
            .obj::<SessionDoc>()
            .one(session_id)
            .await
            .map_err(|e| adk_core::AdkError::session(format!("query failed: {e}")))?
            .ok_or_else(|| adk_core::AdkError::session("session not found"))?;

        let existing_state = &session_doc.state;
        let (_, _, mut session_state) = state_utils::extract_state_deltas(existing_state);

        // Load current app and user state
        let app_state = self.read_app_state(app_name).await?;
        let user_state = self.read_user_state(app_name, user_id).await?;

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let now = event.timestamp;

        // Merge deltas
        let mut new_app_state = app_state;
        new_app_state.extend(app_delta);

        let mut new_user_state = user_state;
        new_user_state.extend(user_delta);

        session_state.extend(session_delta);
        let merged_state =
            state_utils::merge_states(&new_app_state, &new_user_state, &session_state);

        // Build updated documents
        let updated_session = SessionDoc {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            state: merged_state,
            created_at: session_doc.created_at,
            updated_at: now,
        };
        let app_state_doc = AppStateDoc { state: new_app_state, updated_at: now };
        let user_state_doc = UserStateDoc { state: new_user_state, updated_at: now };
        let event_doc = event_to_doc(&event)?;

        let events_parent = self.events_parent(app_name, session_id)?;
        let user_state_parent = self.user_state_parent(app_name, user_id)?;
        let root_col = self.root_collection.clone();

        let mut transaction = self
            .db
            .begin_transaction()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("transaction failed: {e}")))?;

        // Update app state
        self.db
            .fluent()
            .update()
            .in_col(&root_col)
            .document_id(format!("{app_name}/app_state"))
            .object(&app_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Update user state
        self.db
            .fluent()
            .update()
            .in_col("state")
            .document_id("current")
            .parent(&user_state_parent)
            .object(&user_state_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Update session document
        self.db
            .fluent()
            .update()
            .in_col("sessions")
            .document_id(session_id)
            .parent(&sessions_parent)
            .object(&updated_session)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        // Insert event document
        self.db
            .fluent()
            .update()
            .in_col("events")
            .document_id(&event.id)
            .parent(&events_parent)
            .object(&event_doc)
            .add_to_transaction(&mut transaction)
            .map_err(|e| adk_core::AdkError::session(format!("append_event failed: {e}")))?;

        transaction
            .commit()
            .await
            .map_err(|e| adk_core::AdkError::session(format!("commit failed: {e}")))?;

        Ok(())
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let sessions = self
            .list(ListRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                limit: None,
                offset: None,
            })
            .await?;

        for session in &sessions {
            self.delete(DeleteRequest {
                app_name: app_name.to_string(),
                user_id: user_id.to_string(),
                session_id: session.id().to_string(),
            })
            .await?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FirestoreSession — implements Session, State, Events
// ---------------------------------------------------------------------------

struct FirestoreSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for FirestoreSession {
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

impl State for FirestoreSession {
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

impl Events for FirestoreSession {
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
