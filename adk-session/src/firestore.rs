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
//! - App state: `{root}/{app_name}/app_state/current`
//! - User state: `{root}/{app_name}/users/{user_id}/state/current`
//!
//! # Tenant Isolation
//!
//! A session document is addressed by `(app_name, session_id)`; the owning `user_id` is a
//! field of the document. Every operation that addresses a session checks that the document's
//! `app_name`, `user_id`, and `session_id` match the request, and every write re-reads the
//! document inside its transaction before applying anything:
//!
//! | Operation | Session missing, or owned by another user |
//! |-----------|-------------------------------------------|
//! | `create` | Fails with `session.already_exists` when the ID is taken in the app |
//! | `get`, `append_event_for_identity` | Fails with `session.not_found` |
//! | `delete` | No-op, identical to deleting a missing session |
//! | `list`, `delete_all_sessions` | Only the caller's own sessions are returned or removed |
//!
//! A new session never inherits events: `create` removes any events left under its path.
//! App names, user IDs, and session IDs are document IDs, so they must not contain `/`.

use crate::service::session_not_found_for;
use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State, state_utils,
};
use adk_core::identity::SessionId;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use firestore::errors::FirestoreError;
use firestore::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

const DEFAULT_ROOT_COLLECTION: &str = "adk_sessions";
const SESSIONS_COLLECTION: &str = "sessions";
const EVENTS_COLLECTION: &str = "events";
const APP_STATE_COLLECTION: &str = "app_state";
const USERS_COLLECTION: &str = "users";
const USER_STATE_COLLECTION: &str = "state";
const STATE_DOCUMENT_ID: &str = "current";

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
/// Data is organized using subcollections under a configurable root collection. See the
/// [module documentation](self) for the layout and the tenant-isolation guarantees.
pub struct FirestoreSessionService {
    core: SessionCore<FirestoreStore>,
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
        let db = FirestoreDb::new(&config.project_id)
            .await
            .map_err(|e| AdkError::session(format!("firestore connection failed: {e}")))?;
        Ok(Self { core: SessionCore { store: FirestoreStore { db, root_collection } } })
    }

    /// Returns a reference to the underlying `FirestoreDb`.
    pub fn db(&self) -> &FirestoreDb {
        &self.core.store.db
    }

    /// Returns the root collection prefix.
    pub fn root_collection(&self) -> &str {
        &self.core.store.root_collection
    }
}

/// Generate the Firestore document path for a session.
///
/// Path format: `{root}/{app_name}/sessions/{session_id}`
pub fn session_path(root: &str, app_name: &str, session_id: &str) -> String {
    format!("{root}/{app_name}/{SESSIONS_COLLECTION}/{session_id}")
}

/// Generate the Firestore document path for an event within a session.
///
/// Path format: `{root}/{app_name}/sessions/{session_id}/events/{event_id}`
pub fn event_path(root: &str, app_name: &str, session_id: &str, event_id: &str) -> String {
    format!("{root}/{app_name}/{SESSIONS_COLLECTION}/{session_id}/{EVENTS_COLLECTION}/{event_id}")
}

/// Generate the Firestore document path for app-level state.
///
/// Path format: `{root}/{app_name}/app_state/current`
pub fn app_state_path(root: &str, app_name: &str) -> String {
    format!("{root}/{app_name}/{APP_STATE_COLLECTION}/{STATE_DOCUMENT_ID}")
}

/// Generate the Firestore document path for user-level state.
///
/// Path format: `{root}/{app_name}/users/{user_id}/state/current`
pub fn user_state_path(root: &str, app_name: &str, user_id: &str) -> String {
    format!(
        "{root}/{app_name}/{USERS_COLLECTION}/{user_id}/{USER_STATE_COLLECTION}/{STATE_DOCUMENT_ID}"
    )
}

// ---------------------------------------------------------------------------
// Firestore document models
// ---------------------------------------------------------------------------

/// Firestore document for a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct AppStateDoc {
    state: HashMap<String, Value>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: DateTime<Utc>,
}

/// Firestore document for user-level state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct UserStateDoc {
    state: HashMap<String, Value>,
    #[serde(with = "firestore::serialize_as_timestamp")]
    updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Helpers: Event <-> EventDoc, identity checks, errors
// ---------------------------------------------------------------------------

fn event_to_doc(event: &Event) -> Result<EventDoc> {
    let llm_response = serde_json::to_value(&event.llm_response)
        .map_err(|e| AdkError::session(format!("serialize failed: {e}")))?;
    let actions = serde_json::to_value(&event.actions)
        .map_err(|e| AdkError::session(format!("serialize failed: {e}")))?;
    let long_running_tool_ids = serde_json::to_value(&event.long_running_tool_ids)
        .map_err(|e| AdkError::session(format!("serialize failed: {e}")))?;

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

fn doc_to_event(doc: &EventDoc) -> std::result::Result<Event, serde_json::Error> {
    Ok(Event {
        id: doc.id.clone(),
        timestamp: doc.timestamp,
        invocation_id: doc.invocation_id.clone(),
        branch: doc.branch.clone(),
        author: doc.author.clone(),
        llm_request: None,
        llm_response: serde_json::from_value(doc.llm_response.clone())?,
        actions: serde_json::from_value(doc.actions.clone())?,
        long_running_tool_ids: serde_json::from_value(doc.long_running_tool_ids.clone())?,
        provider_metadata: HashMap::new(),
    })
}

/// Converts stored event documents, logging and skipping any that no longer deserialize
/// so one corrupt event does not hide the rest of the session history.
fn decode_event_docs(docs: &[EventDoc], session_id: &str) -> Vec<Event> {
    docs.iter()
        .filter_map(|doc| match doc_to_event(doc) {
            Ok(event) => Some(event),
            Err(error) => {
                tracing::warn!(
                    session.id = %session_id,
                    event.id = %doc.id,
                    error = %error,
                    "skipping stored event that failed to deserialize"
                );
                None
            }
        })
        .collect()
}

/// Returns `true` when `doc` is the session addressed by `(app_name, user_id, session_id)`.
fn is_owned_by(doc: &SessionDoc, app_name: &str, user_id: &str, session_id: &str) -> bool {
    doc.app_name == app_name && doc.user_id == user_id && doc.session_id == session_id
}

/// Rejects identifiers that cannot be used as a single Firestore document ID.
fn validate_path_segment(kind: &str, value: &str) -> Result<()> {
    let reserved = value.len() >= 4 && value.starts_with("__") && value.ends_with("__");
    if value.is_empty() || value.contains('/') || value == "." || value == ".." || reserved {
        return Err(AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::InvalidInput,
            "session.firestore.invalid_identifier",
            format!(
                "{kind} '{value}' cannot be used as a Firestore document ID; it must be \
                 non-empty, must not contain '/', and must not be '.', '..' or match '__.*__'"
            ),
        ));
    }
    Ok(())
}

fn validate_identity(app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
    validate_path_segment("app_name", app_name)?;
    validate_path_segment("user_id", user_id)?;
    validate_path_segment("session_id", session_id)
}

fn session_already_exists(app_name: &str, session_id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::InvalidInput,
        "session.already_exists",
        format!(
            "session '{session_id}' already exists in app '{app_name}'; omit session_id to \
             generate a new one or choose a different ID"
        ),
    )
}

fn firestore_error(context: &str, error: FirestoreError) -> AdkError {
    AdkError::session(format!("{context}: {error}"))
}

// ---------------------------------------------------------------------------
// Storage abstraction
//
// `SessionStore` isolates the handful of Firestore calls the service makes, so the
// ownership and precondition logic in `SessionCore` is unit-tested against an in-memory
// store. `FirestoreStore` is the production implementation.
// ---------------------------------------------------------------------------

/// Precondition the store re-checks inside the write transaction before applying a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionGuard {
    /// The session document must not exist.
    Absent,
    /// The session document must exist and match the commit's app, user, and session ID.
    Owned,
}

/// What a commit does to the session document.
#[derive(Debug, Clone, PartialEq)]
enum SessionWrite {
    /// Write the document, replacing the current one.
    Put(SessionDoc),
    /// Merge keys into the session state of the document read inside the transaction.
    Merge(StateMerge),
    /// Delete the document together with its `events` subcollection.
    Delete,
}

/// Keys merged into a stored state map inside the commit transaction, so a concurrent
/// writer's keys are read and kept rather than overwritten.
#[derive(Debug, Clone, PartialEq)]
struct StateMerge {
    delta: HashMap<String, Value>,
    updated_at: DateTime<Utc>,
}

impl StateMerge {
    /// Returns `None` for an empty delta, which leaves the stored document untouched.
    fn non_empty(delta: HashMap<String, Value>, updated_at: DateTime<Utc>) -> Option<Self> {
        (!delta.is_empty()).then_some(Self { delta, updated_at })
    }

    /// Applies the merge to `stored`.
    ///
    /// Session documents written by earlier releases also hold a copy of the app and user
    /// tiers; `session_tier` drops it.
    fn apply(&self, stored: &HashMap<String, Value>, session_tier: bool) -> HashMap<String, Value> {
        let mut state =
            if session_tier { state_utils::extract_state_deltas(stored).2 } else { stored.clone() };
        state.extend(self.delta.clone());
        state
    }
}

/// One atomic write against a single session, applied by [`SessionStore::commit`].
#[derive(Debug, Clone, PartialEq)]
struct SessionCommit {
    app_name: String,
    user_id: String,
    session_id: String,
    guard: SessionGuard,
    session: SessionWrite,
    app_state: Option<StateMerge>,
    user_state: Option<StateMerge>,
    event: Option<EventDoc>,
}

impl SessionCommit {
    /// Whether the commit removes the events stored under the session path: a create
    /// clears orphans so the new session starts empty, and a delete removes them all.
    fn purges_events(&self) -> bool {
        match (self.guard, &self.session) {
            (SessionGuard::Absent, _) | (_, SessionWrite::Delete) => true,
            (SessionGuard::Owned, SessionWrite::Put(_) | SessionWrite::Merge(_)) => false,
        }
    }
}

/// Result of [`SessionStore::commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitOutcome {
    /// Every write was applied.
    Committed,
    /// The guard did not hold; nothing was written.
    GuardFailed,
}

/// The Firestore operations the session service relies on.
#[async_trait]
trait SessionStore: Send + Sync {
    /// Reads the session document at `{root}/{app_name}/sessions/{session_id}`.
    async fn session(&self, app_name: &str, session_id: &str) -> Result<Option<SessionDoc>>;
    /// Finds session documents whose `session_id` field matches, across every app.
    async fn sessions_with_id(&self, session_id: &str) -> Result<Vec<SessionDoc>>;
    /// Lists the session documents in `app_name` whose `user_id` field matches.
    async fn user_sessions(&self, app_name: &str, user_id: &str) -> Result<Vec<SessionDoc>>;
    /// Reads a session's event documents ordered by timestamp.
    async fn events(&self, app_name: &str, session_id: &str) -> Result<Vec<EventDoc>>;
    /// Reads app-level state, empty when none has been written.
    async fn app_state(&self, app_name: &str) -> Result<HashMap<String, Value>>;
    /// Reads user-level state, empty when none has been written.
    async fn user_state(&self, app_name: &str, user_id: &str) -> Result<HashMap<String, Value>>;
    /// Applies `commit` atomically when its guard holds inside the transaction.
    async fn commit(&self, commit: SessionCommit) -> Result<CommitOutcome>;
}

/// Production [`SessionStore`] backed by a [`FirestoreDb`].
struct FirestoreStore {
    db: FirestoreDb,
    root_collection: String,
}

impl FirestoreStore {
    /// Parent path of the per-app subcollections: `{root}/{app_name}`.
    fn app_parent(&self, app_name: &str) -> Result<String> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| firestore_error("path error", e))?;
        Ok(parent.to_string())
    }

    /// Parent path of a session's events: `{root}/{app_name}/sessions/{session_id}`.
    fn events_parent(&self, app_name: &str, session_id: &str) -> Result<String> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| firestore_error("path error", e))?
            .at(SESSIONS_COLLECTION, session_id)
            .map_err(|e| firestore_error("path error", e))?;
        Ok(parent.to_string())
    }

    /// Parent path of a user's state: `{root}/{app_name}/users/{user_id}`.
    fn user_parent(&self, app_name: &str, user_id: &str) -> Result<String> {
        let parent = self
            .db
            .parent_path(&self.root_collection, app_name)
            .map_err(|e| firestore_error("path error", e))?
            .at(USERS_COLLECTION, user_id)
            .map_err(|e| firestore_error("path error", e))?;
        Ok(parent.to_string())
    }
}

/// Returns the last segment of a Firestore document name, which is the document ID.
fn document_id(document: &FirestoreDocument) -> &str {
    document.name.rsplit('/').next().unwrap_or_default()
}

#[async_trait]
impl SessionStore for FirestoreStore {
    async fn session(&self, app_name: &str, session_id: &str) -> Result<Option<SessionDoc>> {
        let parent = self.app_parent(app_name)?;
        self.db
            .fluent()
            .select()
            .by_id_in(SESSIONS_COLLECTION)
            .parent(&parent)
            .obj::<SessionDoc>()
            .one(session_id)
            .await
            .map_err(|e| firestore_error("query failed", e))
    }

    async fn sessions_with_id(&self, session_id: &str) -> Result<Vec<SessionDoc>> {
        // Collection-group query over every `sessions` collection in the database.
        self.db
            .fluent()
            .select()
            .from(SESSIONS_COLLECTION)
            .parent(self.db.get_documents_path())
            .all_descendants()
            .filter(|q| q.for_all([q.field("session_id").eq(session_id)]))
            .obj::<SessionDoc>()
            .query()
            .await
            .map_err(|e| firestore_error("query failed", e))
    }

    async fn user_sessions(&self, app_name: &str, user_id: &str) -> Result<Vec<SessionDoc>> {
        let parent = self.app_parent(app_name)?;
        self.db
            .fluent()
            .select()
            .from(SESSIONS_COLLECTION)
            .parent(&parent)
            .filter(|q| q.for_all([q.field("user_id").eq(user_id)]))
            .obj::<SessionDoc>()
            .query()
            .await
            .map_err(|e| firestore_error("query failed", e))
    }

    async fn events(&self, app_name: &str, session_id: &str) -> Result<Vec<EventDoc>> {
        let parent = self.events_parent(app_name, session_id)?;
        // Raw documents are decoded one by one so a single corrupt event is skipped
        // instead of failing the whole query.
        let documents = self
            .db
            .fluent()
            .select()
            .from(EVENTS_COLLECTION)
            .parent(&parent)
            .order_by([("timestamp".to_string(), FirestoreQueryDirection::Ascending)])
            .query()
            .await
            .map_err(|e| firestore_error("query failed", e))?;

        Ok(documents
            .iter()
            .filter_map(|document| match FirestoreDb::deserialize_doc_to::<EventDoc>(document) {
                Ok(doc) => Some(doc),
                Err(error) => {
                    tracing::warn!(
                        session.id = %session_id,
                        event.id = %document_id(document),
                        error = %error,
                        "skipping stored event that failed to deserialize"
                    );
                    None
                }
            })
            .collect())
    }

    async fn app_state(&self, app_name: &str) -> Result<HashMap<String, Value>> {
        let parent = self.app_parent(app_name)?;
        let doc: Option<AppStateDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(APP_STATE_COLLECTION)
            .parent(&parent)
            .obj::<AppStateDoc>()
            .one(STATE_DOCUMENT_ID)
            .await
            .map_err(|e| firestore_error("query failed", e))?;
        Ok(doc.map(|d| d.state).unwrap_or_default())
    }

    async fn user_state(&self, app_name: &str, user_id: &str) -> Result<HashMap<String, Value>> {
        let parent = self.user_parent(app_name, user_id)?;
        let doc: Option<UserStateDoc> = self
            .db
            .fluent()
            .select()
            .by_id_in(USER_STATE_COLLECTION)
            .parent(&parent)
            .obj::<UserStateDoc>()
            .one(STATE_DOCUMENT_ID)
            .await
            .map_err(|e| firestore_error("query failed", e))?;
        Ok(doc.map(|d| d.state).unwrap_or_default())
    }

    async fn commit(&self, commit: SessionCommit) -> Result<CommitOutcome> {
        let app_parent = self.app_parent(&commit.app_name)?;
        let events_parent = self.events_parent(&commit.app_name, &commit.session_id)?;
        let user_parent = self.user_parent(&commit.app_name, &commit.user_id)?;

        let mut transaction = self
            .db
            .begin_transaction()
            .await
            .map_err(|e| firestore_error("transaction failed", e))?;
        // Reads through this handle join the transaction, so the guard check and the writes
        // below apply to the same document version.
        let tx_db = self.db.clone_with_consistency_selector(
            FirestoreConsistencySelector::Transaction(transaction.transaction_id().clone()),
        );

        let staged: Result<bool> = async {
            let current: Option<SessionDoc> = tx_db
                .fluent()
                .select()
                .by_id_in(SESSIONS_COLLECTION)
                .parent(&app_parent)
                .obj::<SessionDoc>()
                .one(&commit.session_id)
                .await
                .map_err(|e| firestore_error("query failed", e))?;
            let guard_holds = match commit.guard {
                SessionGuard::Absent => current.is_none(),
                SessionGuard::Owned => current.as_ref().is_some_and(|doc| {
                    is_owned_by(doc, &commit.app_name, &commit.user_id, &commit.session_id)
                }),
            };
            if !guard_holds {
                return Ok(false);
            }

            if commit.purges_events() {
                let stored_events = tx_db
                    .fluent()
                    .select()
                    .from(EVENTS_COLLECTION)
                    .parent(&events_parent)
                    .query()
                    .await
                    .map_err(|e| firestore_error("query failed", e))?;
                for document in &stored_events {
                    self.db
                        .fluent()
                        .delete()
                        .from(EVENTS_COLLECTION)
                        .parent(&events_parent)
                        .document_id(document_id(document))
                        .add_to_transaction(&mut transaction)
                        .map_err(|e| firestore_error("delete failed", e))?;
                }
            }

            // Reads through `tx_db` hold these documents until the commit, so the merges
            // below apply to the stored state rather than a copy read earlier.
            let app_state = match &commit.app_state {
                Some(merge) => {
                    let stored = tx_db
                        .fluent()
                        .select()
                        .by_id_in(APP_STATE_COLLECTION)
                        .parent(&app_parent)
                        .obj::<AppStateDoc>()
                        .one(STATE_DOCUMENT_ID)
                        .await
                        .map_err(|e| firestore_error("query failed", e))?
                        .map(|doc| doc.state)
                        .unwrap_or_default();
                    Some(AppStateDoc {
                        state: merge.apply(&stored, false),
                        updated_at: merge.updated_at,
                    })
                }
                None => None,
            };
            let user_state = match &commit.user_state {
                Some(merge) => {
                    let stored = tx_db
                        .fluent()
                        .select()
                        .by_id_in(USER_STATE_COLLECTION)
                        .parent(&user_parent)
                        .obj::<UserStateDoc>()
                        .one(STATE_DOCUMENT_ID)
                        .await
                        .map_err(|e| firestore_error("query failed", e))?
                        .map(|doc| doc.state)
                        .unwrap_or_default();
                    Some(UserStateDoc {
                        state: merge.apply(&stored, false),
                        updated_at: merge.updated_at,
                    })
                }
                None => None,
            };

            let session_doc = match (&commit.session, current) {
                (SessionWrite::Put(doc), _) => Some(doc.clone()),
                (SessionWrite::Merge(merge), Some(current)) => Some(SessionDoc {
                    state: merge.apply(&current.state, true),
                    updated_at: merge.updated_at,
                    ..current
                }),
                // The `Owned` guard above guarantees a current document.
                (SessionWrite::Merge(_), None) => return Ok(false),
                (SessionWrite::Delete, _) => None,
            };

            match &session_doc {
                Some(doc) => {
                    // Backs up the in-transaction guard check at commit time.
                    let precondition = match commit.guard {
                        SessionGuard::Absent => FirestoreWritePrecondition::Exists(false),
                        SessionGuard::Owned => FirestoreWritePrecondition::Exists(true),
                    };
                    self.db
                        .fluent()
                        .update()
                        .in_col(SESSIONS_COLLECTION)
                        .precondition(precondition)
                        .document_id(&commit.session_id)
                        .parent(&app_parent)
                        .object(doc)
                        .add_to_transaction(&mut transaction)
                        .map_err(|e| firestore_error("write failed", e))?;
                }
                None => {
                    self.db
                        .fluent()
                        .delete()
                        .from(SESSIONS_COLLECTION)
                        .parent(&app_parent)
                        .document_id(&commit.session_id)
                        .add_to_transaction(&mut transaction)
                        .map_err(|e| firestore_error("delete failed", e))?;
                }
            }

            if let Some(app_state) = &app_state {
                self.db
                    .fluent()
                    .update()
                    .in_col(APP_STATE_COLLECTION)
                    .document_id(STATE_DOCUMENT_ID)
                    .parent(&app_parent)
                    .object(app_state)
                    .add_to_transaction(&mut transaction)
                    .map_err(|e| firestore_error("write failed", e))?;
            }
            if let Some(user_state) = &user_state {
                self.db
                    .fluent()
                    .update()
                    .in_col(USER_STATE_COLLECTION)
                    .document_id(STATE_DOCUMENT_ID)
                    .parent(&user_parent)
                    .object(user_state)
                    .add_to_transaction(&mut transaction)
                    .map_err(|e| firestore_error("write failed", e))?;
            }
            if let Some(event) = &commit.event {
                self.db
                    .fluent()
                    .update()
                    .in_col(EVENTS_COLLECTION)
                    .document_id(&event.id)
                    .parent(&events_parent)
                    .object(event)
                    .add_to_transaction(&mut transaction)
                    .map_err(|e| firestore_error("write failed", e))?;
            }
            Ok(true)
        }
        .await;

        match staged {
            Ok(true) => match transaction.commit().await {
                Ok(_) => Ok(CommitOutcome::Committed),
                Err(
                    FirestoreError::DataConflictError(_) | FirestoreError::DataNotFoundError(_),
                ) => Ok(CommitOutcome::GuardFailed),
                Err(error) => Err(firestore_error("commit failed", error)),
            },
            Ok(false) => {
                if let Err(error) = transaction.rollback().await {
                    tracing::warn!(error = %error, "firestore transaction rollback failed");
                }
                Ok(CommitOutcome::GuardFailed)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    tracing::warn!(error = %rollback_error, "firestore transaction rollback failed");
                }
                Err(error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Session logic
// ---------------------------------------------------------------------------

/// Session semantics over a [`SessionStore`]: identity checks, state tiers, and guards.
struct SessionCore<S> {
    store: S,
}

impl<S: SessionStore> SessionCore<S> {
    /// Appends `event` to the session owned by `(app_name, user_id, session_id)`.
    async fn append(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        mut event: Event,
    ) -> Result<()> {
        event.actions.state_delta.retain(|k, _| !k.starts_with(KEY_PREFIX_TEMP));
        let not_found = || session_not_found_for(app_name, user_id, session_id);

        self.store
            .session(app_name, session_id)
            .await?
            .filter(|doc| is_owned_by(doc, app_name, user_id, session_id))
            .ok_or_else(not_found)?;

        let (app_delta, user_delta, session_delta) =
            state_utils::extract_state_deltas(&event.actions.state_delta);

        let now = event.timestamp;
        let commit = SessionCommit {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            guard: SessionGuard::Owned,
            session: SessionWrite::Merge(StateMerge { delta: session_delta, updated_at: now }),
            app_state: StateMerge::non_empty(app_delta, now),
            user_state: StateMerge::non_empty(user_delta, now),
            event: Some(event_to_doc(&event)?),
        };

        match self.store.commit(commit).await? {
            CommitOutcome::Committed => Ok(()),
            CommitOutcome::GuardFailed => Err(not_found()),
        }
    }
}

#[async_trait]
impl<S: SessionStore> SessionService for SessionCore<S> {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        let session_id = req.session_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string());
        req.try_app_name()?;
        req.try_user_id()?;
        SessionId::try_from(session_id.as_str())?;
        validate_identity(&req.app_name, &req.user_id, &session_id)?;

        let (app_delta, user_delta, session_state) = state_utils::extract_state_deltas(&req.state);

        let now = Utc::now();
        let commit = SessionCommit {
            app_name: req.app_name.clone(),
            user_id: req.user_id.clone(),
            session_id: session_id.clone(),
            guard: SessionGuard::Absent,
            session: SessionWrite::Put(SessionDoc {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                session_id: session_id.clone(),
                state: session_state.clone(),
                created_at: now,
                updated_at: now,
            }),
            app_state: StateMerge::non_empty(app_delta, now),
            user_state: StateMerge::non_empty(user_delta, now),
            event: None,
        };

        match self.store.commit(commit).await? {
            CommitOutcome::Committed => {
                let app_state = self.store.app_state(&req.app_name).await?;
                let user_state = self.store.user_state(&req.app_name, &req.user_id).await?;
                Ok(Box::new(FirestoreSession {
                    app_name: req.app_name,
                    user_id: req.user_id,
                    session_id,
                    state: state_utils::merge_states(&app_state, &user_state, &session_state),
                    events: Vec::new(),
                    updated_at: now,
                }))
            }
            CommitOutcome::GuardFailed => Err(session_already_exists(&req.app_name, &session_id)),
        }
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        validate_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let session_doc = self
            .store
            .session(&req.app_name, &req.session_id)
            .await?
            .filter(|doc| is_owned_by(doc, &req.app_name, &req.user_id, &req.session_id))
            .ok_or_else(|| crate::service::session_not_found(&req))?;

        let app_state = self.store.app_state(&req.app_name).await?;
        let user_state = self.store.user_state(&req.app_name, &req.user_id).await?;
        let event_docs = self.store.events(&req.app_name, &req.session_id).await?;
        let mut events = decode_event_docs(&event_docs, &req.session_id);

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
            state: state_utils::merge_current_tiers(&app_state, &user_state, &session_doc.state),
            events,
            updated_at: session_doc.updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        req.try_app_name()?;
        req.try_user_id()?;
        validate_path_segment("app_name", &req.app_name)?;
        validate_path_segment("user_id", &req.user_id)?;

        let mut docs: Vec<SessionDoc> = self
            .store
            .user_sessions(&req.app_name, &req.user_id)
            .await?
            .into_iter()
            .filter(|doc| doc.app_name == req.app_name && doc.user_id == req.user_id)
            .collect();
        docs.sort_by_key(|doc| std::cmp::Reverse(doc.updated_at));

        let app_state = self.store.app_state(&req.app_name).await?;
        let user_state = self.store.user_state(&req.app_name, &req.user_id).await?;

        Ok(docs
            .into_iter()
            .skip(req.offset.unwrap_or(0))
            .take(req.limit.unwrap_or(usize::MAX))
            .map(|doc| {
                Box::new(FirestoreSession {
                    app_name: doc.app_name,
                    user_id: doc.user_id,
                    session_id: doc.session_id,
                    state: state_utils::merge_current_tiers(&app_state, &user_state, &doc.state),
                    events: Vec::new(),
                    updated_at: doc.updated_at,
                }) as Box<dyn Session>
            })
            .collect())
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        validate_identity(&req.app_name, &req.user_id, &req.session_id)?;

        let commit = SessionCommit {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            guard: SessionGuard::Owned,
            session: SessionWrite::Delete,
            app_state: None,
            user_state: None,
            event: None,
        };
        // A missing or foreign session is a no-op, so deletion reveals nothing about
        // sessions the caller does not own.
        match self.store.commit(commit).await? {
            CommitOutcome::Committed | CommitOutcome::GuardFailed => Ok(()),
        }
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        SessionId::try_from(session_id)?;
        validate_path_segment("session_id", session_id)?;

        let mut owners: Vec<(String, String)> = self
            .store
            .sessions_with_id(session_id)
            .await?
            .into_iter()
            .filter(|doc| doc.session_id == session_id)
            .map(|doc| (doc.app_name, doc.user_id))
            .collect();
        owners.sort();
        owners.dedup();

        match owners.as_slice() {
            [] => Err(AdkError::not_found(
                ErrorComponent::Session,
                "session.not_found",
                format!("session '{session_id}' was not found"),
            )),
            [(app_name, user_id)] => self.append(app_name, user_id, session_id, event).await,
            _ => Err(AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::InvalidInput,
                "session.ambiguous_id",
                format!(
                    "session ID '{session_id}' exists for more than one app or user; use \
                     append_event_for_identity to address the session unambiguously"
                ),
            )),
        }
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let app_name = req.identity.app_name.as_ref();
        let user_id = req.identity.user_id.as_ref();
        let session_id = req.identity.session_id.as_ref();
        validate_identity(app_name, user_id, session_id)?;
        self.append(app_name, user_id, session_id, req.event).await
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
// SessionService implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl SessionService for FirestoreSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        self.core.create(req).await
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        self.core.get(req).await
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        self.core.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.core.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        self.core.append_event(session_id, event).await
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        self.core.append_event_for_identity(req).await
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        self.core.delete_all_sessions(app_name, user_id).await
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

// ---------------------------------------------------------------------------
// Tests
//
// These exercise `SessionCore` over an in-memory `SessionStore` that models the Firestore
// layout: session documents keyed by `(app_name, session_id)` and event subcollections that
// outlive their parent document. `FirestoreStore` itself (transactional reads, write
// preconditions, collection-group queries, document paths) needs the Firestore emulator.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::identity::{AdkIdentity, AppName, UserId};
    use serde_json::json;
    use std::sync::Mutex;

    type SessionKey = (String, String);
    type CommitHook = Box<dyn FnOnce(&mut FakeData) + Send>;

    #[derive(Default)]
    struct FakeData {
        sessions: HashMap<SessionKey, SessionDoc>,
        events: HashMap<SessionKey, Vec<EventDoc>>,
        app_states: HashMap<String, AppStateDoc>,
        user_states: HashMap<SessionKey, UserStateDoc>,
    }

    #[derive(Default)]
    struct FakeStore {
        data: Mutex<FakeData>,
        /// Runs once at the start of the next commit, to simulate a concurrent writer.
        before_commit: Mutex<Option<CommitHook>>,
    }

    fn key(app_name: &str, session_id: &str) -> SessionKey {
        (app_name.to_string(), session_id.to_string())
    }

    #[async_trait]
    impl SessionStore for FakeStore {
        async fn session(&self, app_name: &str, session_id: &str) -> Result<Option<SessionDoc>> {
            Ok(self.data.lock().unwrap().sessions.get(&key(app_name, session_id)).cloned())
        }

        async fn sessions_with_id(&self, session_id: &str) -> Result<Vec<SessionDoc>> {
            let data = self.data.lock().unwrap();
            Ok(data.sessions.values().filter(|doc| doc.session_id == session_id).cloned().collect())
        }

        async fn user_sessions(&self, app_name: &str, user_id: &str) -> Result<Vec<SessionDoc>> {
            let data = self.data.lock().unwrap();
            Ok(data
                .sessions
                .iter()
                .filter(|((app, _), doc)| app == app_name && doc.user_id == user_id)
                .map(|(_, doc)| doc.clone())
                .collect())
        }

        async fn events(&self, app_name: &str, session_id: &str) -> Result<Vec<EventDoc>> {
            let data = self.data.lock().unwrap();
            let mut events =
                data.events.get(&key(app_name, session_id)).cloned().unwrap_or_default();
            events.sort_by_key(|event| event.timestamp);
            Ok(events)
        }

        async fn app_state(&self, app_name: &str) -> Result<HashMap<String, Value>> {
            let data = self.data.lock().unwrap();
            Ok(data.app_states.get(app_name).map(|doc| doc.state.clone()).unwrap_or_default())
        }

        async fn user_state(
            &self,
            app_name: &str,
            user_id: &str,
        ) -> Result<HashMap<String, Value>> {
            let data = self.data.lock().unwrap();
            Ok(data
                .user_states
                .get(&key(app_name, user_id))
                .map(|doc| doc.state.clone())
                .unwrap_or_default())
        }

        async fn commit(&self, commit: SessionCommit) -> Result<CommitOutcome> {
            let hook = self.before_commit.lock().unwrap().take();
            let mut data = self.data.lock().unwrap();
            if let Some(hook) = hook {
                hook(&mut data);
            }

            let session_key = key(&commit.app_name, &commit.session_id);
            let guard_holds = match commit.guard {
                SessionGuard::Absent => !data.sessions.contains_key(&session_key),
                SessionGuard::Owned => data.sessions.get(&session_key).is_some_and(|doc| {
                    is_owned_by(doc, &commit.app_name, &commit.user_id, &commit.session_id)
                }),
            };
            if !guard_holds {
                return Ok(CommitOutcome::GuardFailed);
            }

            if commit.purges_events() {
                data.events.remove(&session_key);
            }
            match commit.session {
                SessionWrite::Put(doc) => {
                    data.sessions.insert(session_key.clone(), doc);
                }
                SessionWrite::Merge(merge) => {
                    let doc = data.sessions.get_mut(&session_key).expect("guarded by Owned");
                    doc.state = merge.apply(&doc.state, true);
                    doc.updated_at = merge.updated_at;
                }
                SessionWrite::Delete => {
                    data.sessions.remove(&session_key);
                }
            }
            if let Some(merge) = commit.app_state {
                let stored = data.app_states.get(&commit.app_name).map(|doc| doc.state.clone());
                let state = merge.apply(&stored.unwrap_or_default(), false);
                data.app_states.insert(
                    commit.app_name.clone(),
                    AppStateDoc { state, updated_at: merge.updated_at },
                );
            }
            if let Some(merge) = commit.user_state {
                let user_key = key(&commit.app_name, &commit.user_id);
                let stored = data.user_states.get(&user_key).map(|doc| doc.state.clone());
                let state = merge.apply(&stored.unwrap_or_default(), false);
                data.user_states
                    .insert(user_key, UserStateDoc { state, updated_at: merge.updated_at });
            }
            if let Some(event) = commit.event {
                data.events.entry(session_key).or_default().push(event);
            }
            Ok(CommitOutcome::Committed)
        }
    }

    fn service() -> SessionCore<FakeStore> {
        SessionCore { store: FakeStore::default() }
    }

    fn create_req(app: &str, user: &str, session: &str, state: Value) -> CreateRequest {
        CreateRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: Some(session.to_string()),
            state: serde_json::from_value(state).unwrap(),
        }
    }

    fn get_req(app: &str, user: &str, session: &str) -> GetRequest {
        GetRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: None,
            after: None,
        }
    }

    fn delete_req(app: &str, user: &str, session: &str) -> DeleteRequest {
        DeleteRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
        }
    }

    fn append_req(app: &str, user: &str, session: &str, event: Event) -> AppendEventRequest {
        AppendEventRequest {
            identity: AdkIdentity::new(
                AppName::try_from(app).unwrap(),
                UserId::try_from(user).unwrap(),
                SessionId::try_from(session).unwrap(),
            ),
            event,
        }
    }

    fn event_with_delta(id: &str, delta: Value) -> Event {
        let mut event = Event::with_id(id, "inv-1");
        event.author = "agent".to_string();
        event.actions.state_delta = serde_json::from_value(delta).unwrap();
        event
    }

    /// Snapshot of everything stored under one session path.
    fn stored(
        core: &SessionCore<FakeStore>,
        app: &str,
        session: &str,
    ) -> (Option<SessionDoc>, Vec<EventDoc>) {
        let data = core.store.data.lock().unwrap();
        (
            data.sessions.get(&key(app, session)).cloned(),
            data.events.get(&key(app, session)).cloned().unwrap_or_default(),
        )
    }

    async fn alice_session_with_event(core: &SessionCore<FakeStore>) {
        core.create(create_req("app", "alice", "s1", json!({"secret": "alice-data"})))
            .await
            .unwrap();
        core.append_event_for_identity(append_req(
            "app",
            "alice",
            "s1",
            event_with_delta("e1", json!({"secret": "alice-updated"})),
        ))
        .await
        .unwrap();
    }

    fn assert_error(error: &AdkError, category: ErrorCategory, code: &str) {
        assert_eq!((error.category, error.code), (category, code), "{error}");
    }

    #[tokio::test]
    async fn create_rejects_a_session_id_owned_by_another_user() {
        let core = service();
        alice_session_with_event(&core).await;
        let before = stored(&core, "app", "s1");

        let error = core
            .create(create_req("app", "bob", "s1", json!({"secret": "bob-data"})))
            .await
            .err()
            .expect("taking over another user's session id must fail");
        assert_error(&error, ErrorCategory::InvalidInput, "session.already_exists");

        assert_eq!(stored(&core, "app", "s1"), before);
        let bob_get = core.get(get_req("app", "bob", "s1")).await.err().unwrap();
        assert_error(&bob_get, ErrorCategory::NotFound, "session.not_found");
    }

    #[tokio::test]
    async fn create_rejects_a_duplicate_session_id_for_the_same_user() {
        let core = service();
        alice_session_with_event(&core).await;
        let before = stored(&core, "app", "s1");

        let error = core
            .create(create_req("app", "alice", "s1", json!({})))
            .await
            .err()
            .expect("recreating an existing session must fail");
        assert_error(&error, ErrorCategory::InvalidInput, "session.already_exists");
        assert_eq!(stored(&core, "app", "s1"), before);
    }

    #[tokio::test]
    async fn create_does_not_inherit_orphaned_events() {
        let core = service();
        let orphan = event_to_doc(&event_with_delta("orphan", json!({}))).unwrap();
        core.store.data.lock().unwrap().events.insert(key("app", "s1"), vec![orphan]);

        core.create(create_req("app", "bob", "s1", json!({}))).await.unwrap();

        let session = core.get(get_req("app", "bob", "s1")).await.unwrap();
        assert_eq!(session.events().len(), 0);
    }

    #[tokio::test]
    async fn delete_by_another_user_leaves_the_session_untouched() {
        let core = service();
        alice_session_with_event(&core).await;
        let before = stored(&core, "app", "s1");

        core.delete(delete_req("app", "bob", "s1")).await.unwrap();
        assert_eq!(stored(&core, "app", "s1"), before);

        core.delete(delete_req("app", "alice", "s1")).await.unwrap();
        assert_eq!(stored(&core, "app", "s1"), (None, Vec::new()));
    }

    #[tokio::test]
    async fn append_with_another_users_identity_is_not_found() {
        let core = service();
        alice_session_with_event(&core).await;
        let before = stored(&core, "app", "s1");

        let error = core
            .append_event_for_identity(append_req(
                "app",
                "bob",
                "s1",
                event_with_delta("e2", json!({"secret": "bob-overwrite"})),
            ))
            .await
            .expect_err("appending to another user's session must fail");
        assert_error(&error, ErrorCategory::NotFound, "session.not_found");
        assert_eq!(stored(&core, "app", "s1"), before);
    }

    #[tokio::test]
    async fn append_rechecks_ownership_inside_the_commit() {
        let core = service();
        core.create(create_req("app", "alice", "s1", json!({}))).await.unwrap();

        // Between the ownership read and the commit, the session is deleted and recreated
        // by another user.
        let takeover = SessionDoc {
            app_name: "app".to_string(),
            user_id: "mallory".to_string(),
            session_id: "s1".to_string(),
            state: serde_json::from_value(json!({"owner": "mallory"})).unwrap(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let replacement = takeover.clone();
        *core.store.before_commit.lock().unwrap() = Some(Box::new(move |data: &mut FakeData| {
            data.sessions.insert(key("app", "s1"), replacement);
        }));

        let error = core
            .append_event_for_identity(append_req(
                "app",
                "alice",
                "s1",
                event_with_delta("e1", json!({"owner": "alice"})),
            ))
            .await
            .expect_err("a commit against a session that changed owner must fail");
        assert_error(&error, ErrorCategory::NotFound, "session.not_found");
        assert_eq!(stored(&core, "app", "s1"), (Some(takeover), Vec::new()));
    }

    #[tokio::test]
    async fn legacy_append_event_resolves_the_unique_owner() {
        let core = service();
        core.create(create_req("app", "alice", "s1", json!({}))).await.unwrap();

        core.append_event("s1", event_with_delta("e1", json!({"k": 1}))).await.unwrap();
        let session = core.get(get_req("app", "alice", "s1")).await.unwrap();
        assert_eq!(session.state().all(), serde_json::from_value(json!({"k": 1})).unwrap());
        assert_eq!(session.events().len(), 1);

        let missing = core.append_event("missing", Event::new("inv")).await.err().unwrap();
        assert_error(&missing, ErrorCategory::NotFound, "session.not_found");

        core.create(create_req("other_app", "bob", "s1", json!({}))).await.unwrap();
        let ambiguous = core.append_event("s1", Event::new("inv")).await.err().unwrap();
        assert_error(&ambiguous, ErrorCategory::InvalidInput, "session.ambiguous_id");
    }

    #[tokio::test]
    async fn list_and_delete_all_are_scoped_to_the_caller() {
        let core = service();
        for (app, user, session) in [
            ("app", "alice", "a1"),
            ("app", "alice", "a2"),
            ("app", "bob", "b1"),
            ("other_app", "alice", "a3"),
        ] {
            core.create(create_req(app, user, session, json!({}))).await.unwrap();
        }
        // A document stored under `app` that claims to belong to another app.
        let forged = SessionDoc {
            app_name: "elsewhere".to_string(),
            user_id: "alice".to_string(),
            session_id: "forged".to_string(),
            state: HashMap::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        core.store.data.lock().unwrap().sessions.insert(key("app", "forged"), forged);

        let list = |app: &'static str, user: &'static str| {
            let core = &core;
            async move {
                let mut ids: Vec<String> = core
                    .list(ListRequest {
                        app_name: app.to_string(),
                        user_id: user.to_string(),
                        limit: None,
                        offset: None,
                    })
                    .await
                    .unwrap()
                    .iter()
                    .map(|session| session.id().to_string())
                    .collect();
                ids.sort();
                ids
            }
        };
        assert_eq!(list("app", "alice").await, vec!["a1", "a2"]);

        core.delete_all_sessions("app", "alice").await.unwrap();
        assert_eq!(list("app", "alice").await, Vec::<String>::new());
        assert_eq!(list("app", "bob").await, vec!["b1"]);
        assert_eq!(list("other_app", "alice").await, vec!["a3"]);
    }

    #[tokio::test]
    async fn state_tiers_round_trip_through_create_append_and_get() {
        let core = service();
        core.create(create_req(
            "app",
            "alice",
            "s1",
            json!({"app:theme": "dark", "user:lang": "sw", "k": 1, "temp:scratch": true}),
        ))
        .await
        .unwrap();
        core.append_event_for_identity(append_req(
            "app",
            "alice",
            "s1",
            event_with_delta("e1", json!({"k": 2, "user:lang": "en", "temp:x": 1})),
        ))
        .await
        .unwrap();

        let session = core.get(get_req("app", "alice", "s1")).await.unwrap();
        assert_eq!(
            session.state().all(),
            serde_json::from_value::<HashMap<String, Value>>(
                json!({"app:theme": "dark", "user:lang": "en", "k": 2})
            )
            .unwrap()
        );
        let delta = session.events().at(0).unwrap().actions.state_delta.clone();
        assert_eq!(
            delta,
            serde_json::from_value::<HashMap<String, Value>>(json!({"k": 2, "user:lang": "en"}))
                .unwrap()
        );
    }

    #[tokio::test]
    async fn get_and_list_serve_the_current_shared_tiers() {
        let core = service();
        core.create(create_req("app", "alice", "s1", json!({"app:theme": "light", "k": 1})))
            .await
            .unwrap();
        core.create(create_req("app", "alice", "s2", json!({}))).await.unwrap();
        core.append_event_for_identity(append_req(
            "app",
            "alice",
            "s2",
            event_with_delta("e1", json!({"app:theme": "dark", "user:lang": "fr"})),
        ))
        .await
        .unwrap();

        let expected: HashMap<String, Value> =
            serde_json::from_value(json!({"app:theme": "dark", "user:lang": "fr", "k": 1}))
                .unwrap();
        let session = core.get(get_req("app", "alice", "s1")).await.unwrap();
        assert_eq!(session.state().all(), expected);

        let listed = core
            .list(ListRequest {
                app_name: "app".to_string(),
                user_id: "alice".to_string(),
                limit: None,
                offset: None,
            })
            .await
            .unwrap();
        let listed = listed.iter().find(|session| session.id() == "s1").unwrap();
        assert_eq!(listed.state().all(), expected);
    }

    #[tokio::test]
    async fn append_keeps_keys_a_concurrent_writer_committed_first() {
        let core = service();
        core.create(create_req("app", "alice", "s1", json!({}))).await.unwrap();

        // Between the append's ownership read and its commit, another writer commits its
        // own key to every tier.
        *core.store.before_commit.lock().unwrap() = Some(Box::new(|data: &mut FakeData| {
            let theirs: HashMap<String, Value> = HashMap::from([("theirs".to_string(), json!(1))]);
            data.app_states.insert(
                "app".to_string(),
                AppStateDoc { state: theirs.clone(), updated_at: Utc::now() },
            );
            data.user_states.insert(
                key("app", "alice"),
                UserStateDoc { state: theirs.clone(), updated_at: Utc::now() },
            );
            data.sessions.get_mut(&key("app", "s1")).unwrap().state = theirs;
        }));

        core.append_event_for_identity(append_req(
            "app",
            "alice",
            "s1",
            event_with_delta("e1", json!({"app:mine": 2, "user:mine": 2, "mine": 2})),
        ))
        .await
        .unwrap();

        let session = core.get(get_req("app", "alice", "s1")).await.unwrap();
        assert_eq!(
            session.state().all(),
            serde_json::from_value::<HashMap<String, Value>>(json!({
                "app:theirs": 1, "app:mine": 2,
                "user:theirs": 1, "user:mine": 2,
                "theirs": 1, "mine": 2,
            }))
            .unwrap()
        );
    }

    #[tokio::test]
    async fn identifiers_that_are_not_single_document_ids_are_rejected() {
        let core = service();
        let error =
            core.create(create_req("app", "alice", "s1/events/e1", json!({}))).await.err().unwrap();
        assert_error(&error, ErrorCategory::InvalidInput, "session.firestore.invalid_identifier");

        for bad in ["", "a/b", ".", "..", "__reserved__"] {
            let error = validate_path_segment("user_id", bad).err().unwrap();
            assert_error(
                &error,
                ErrorCategory::InvalidInput,
                "session.firestore.invalid_identifier",
            );
        }
        validate_path_segment("user_id", "alice@example.com").unwrap();
    }

    #[test]
    fn undecodable_event_documents_are_skipped() {
        let good = event_to_doc(&event_with_delta("good", json!({}))).unwrap();
        let bad = EventDoc { id: "bad".to_string(), actions: json!("not actions"), ..good.clone() };

        let events = decode_event_docs(&[bad, good], "s1");
        let ids: Vec<&str> = events.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(ids, vec!["good"]);
    }

    #[test]
    fn layout_paths_are_document_paths() {
        assert_eq!(app_state_path("root", "app"), "root/app/app_state/current");
        assert_eq!(user_state_path("root", "app", "alice"), "root/app/users/alice/state/current");
        assert_eq!(session_path("root", "app", "s1"), "root/app/sessions/s1");
        assert_eq!(event_path("root", "app", "s1", "e1"), "root/app/sessions/s1/events/e1");
    }
}
