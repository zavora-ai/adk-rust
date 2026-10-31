use crate::{Event, Session};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

/// Request to create a new session.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// Application name that owns the session.
    pub app_name: String,
    /// User identifier for the session owner.
    pub user_id: String,
    /// Optional session ID; generated if not provided.
    pub session_id: Option<String>,
    /// Initial state key-value pairs for the session.
    pub state: HashMap<String, Value>,
}

impl CreateRequest {
    /// Returns the application name as a typed [`AppName`].
    ///
    /// # Errors
    ///
    /// Returns an error if the raw string fails identity validation.
    pub fn try_app_name(&self) -> Result<AppName> {
        Ok(AppName::try_from(self.app_name.as_str())?)
    }

    /// Returns the user identifier as a typed [`UserId`].
    ///
    /// # Errors
    ///
    /// Returns an error if the raw string fails identity validation.
    pub fn try_user_id(&self) -> Result<UserId> {
        Ok(UserId::try_from(self.user_id.as_str())?)
    }

    /// Returns the session identifier as a typed [`SessionId`], if one was
    /// provided.
    ///
    /// Returns `Ok(None)` when `session_id` is `None` (the service will
    /// generate one). Returns an error only when a non-`None` value fails
    /// identity validation.
    ///
    /// # Errors
    ///
    /// Returns an error if the provided session ID string fails validation.
    pub fn try_session_id(&self) -> Result<Option<SessionId>> {
        self.session_id.as_deref().map(SessionId::try_from).transpose().map_err(Into::into)
    }

    /// Returns the stable session-scoped [`AdkIdentity`] triple, if a session
    /// ID was provided.
    ///
    /// Because `CreateRequest` allows `session_id` to be `None` (the backend
    /// generates one), this returns `Ok(None)` when no session ID is present.
    /// The app name and user ID are validated either way, so a backend can call
    /// this once to validate every identifier the request carries.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the constituent identifiers fail validation.
    pub fn try_identity(&self) -> Result<Option<AdkIdentity>> {
        let app_name = self.try_app_name()?;
        let user_id = self.try_user_id()?;
        Ok(self.try_session_id()?.map(|session_id| AdkIdentity { app_name, user_id, session_id }))
    }
}

/// Request to retrieve an existing session.
#[derive(Debug, Clone)]
pub struct GetRequest {
    /// Application name that owns the session.
    pub app_name: String,
    /// User identifier for the session owner.
    pub user_id: String,
    /// Session identifier to retrieve.
    pub session_id: String,
    /// If set, only return the N most recent events.
    pub num_recent_events: Option<usize>,
    /// If set, only return events after this timestamp.
    pub after: Option<DateTime<Utc>>,
}

impl GetRequest {
    /// Returns the stable session-scoped [`AdkIdentity`] triple.
    ///
    /// Parses `app_name`, `user_id`, and `session_id` into their typed
    /// equivalents and combines them into an [`AdkIdentity`].
    ///
    /// # Errors
    ///
    /// Returns an error if any of the three identifiers fail validation.
    pub fn try_identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity {
            app_name: AppName::try_from(self.app_name.as_str())?,
            user_id: UserId::try_from(self.user_id.as_str())?,
            session_id: SessionId::try_from(self.session_id.as_str())?,
        })
    }
}

/// Validates the `(app_name, user_id)` scope addressed by `list` and `delete_all_sessions`.
pub(crate) fn validate_scope(app_name: &str, user_id: &str) -> Result<()> {
    AppName::try_from(app_name)?;
    UserId::try_from(user_id)?;
    Ok(())
}

/// Validates a session ID addressed on its own, as by `append_event`.
pub(crate) fn validate_session_id(session_id: &str) -> Result<()> {
    SessionId::try_from(session_id)?;
    Ok(())
}

/// Builds the canonical error returned when a session identity has no matching record.
pub(crate) fn session_not_found(req: &GetRequest) -> AdkError {
    session_not_found_for(&req.app_name, &req.user_id, &req.session_id)
}

/// Builds the canonical not-found error for a raw `(app_name, user_id, session_id)` triple.
pub(crate) fn session_not_found_for(app_name: &str, user_id: &str, session_id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Session,
        "session.not_found",
        format!("session '{session_id}' was not found for app '{app_name}' and user '{user_id}'"),
    )
}

/// Builds the canonical not-found error for a typed session identity.
pub(crate) fn identity_not_found(identity: &AdkIdentity) -> AdkError {
    session_not_found_for(
        identity.app_name.as_ref(),
        identity.user_id.as_ref(),
        identity.session_id.as_ref(),
    )
}

/// Builds the canonical not-found error for a session addressed by its ID alone.
pub(crate) fn session_id_not_found(session_id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Session,
        "session.not_found",
        format!("session '{session_id}' was not found"),
    )
}

/// Builds the error for a session ID that more than one app or user owns.
pub(crate) fn ambiguous_session_id(session_id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::InvalidInput,
        "session.ambiguous_id",
        format!(
            "session ID '{session_id}' exists for more than one app or user; use \
             append_event_for_identity to address the session unambiguously"
        ),
    )
}

/// Builds the canonical conflict error returned when `create` names an existing session.
pub(crate) fn session_already_exists(app_name: &str, user_id: &str, session_id: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Conflict,
        "session.already_exists",
        format!(
            "session '{session_id}' already exists for app '{app_name}' and user '{user_id}'; \
             read it with get, or omit session_id to create a new one"
        ),
    )
}

/// Builds the error returned when a rewind target is not an event of the session.
pub(crate) fn rewind_event_not_found(identity: &AdkIdentity, event_id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Session,
        "session.event_not_found",
        format!(
            "event '{event_id}' is not in session '{}'; rewind targets an event the session holds",
            identity.session_id
        ),
    )
}

/// Builds the error returned when `rewind_steps` asks for more steps than the session has events.
pub(crate) fn rewind_steps_exceed(identity: &AdkIdentity, steps: usize, events: usize) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::InvalidInput,
        "session.rewind.too_many_steps",
        format!(
            "cannot rewind {steps} steps: session '{}' holds {events} events",
            identity.session_id
        ),
    )
}

fn rewind_unsupported() -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unsupported,
        "session.rewind.unsupported",
        "this session backend does not support rewind; the in-memory, SQLite, and encrypted \
         backends do",
    )
}

/// Request to list sessions for a given app and user.
#[derive(Debug, Clone)]
pub struct ListRequest {
    /// Application name to filter sessions by.
    pub app_name: String,
    /// User identifier to filter sessions by.
    pub user_id: String,
    /// Maximum number of sessions to return. `None` means no limit.
    pub limit: Option<usize>,
    /// Number of sessions to skip for pagination. `None` means start from the beginning.
    pub offset: Option<usize>,
}

impl ListRequest {
    /// Returns the application name as a typed [`AppName`].
    ///
    /// # Errors
    ///
    /// Returns an error if the raw string fails identity validation.
    pub fn try_app_name(&self) -> Result<AppName> {
        Ok(AppName::try_from(self.app_name.as_str())?)
    }

    /// Returns the user identifier as a typed [`UserId`].
    ///
    /// # Errors
    ///
    /// Returns an error if the raw string fails identity validation.
    pub fn try_user_id(&self) -> Result<UserId> {
        Ok(UserId::try_from(self.user_id.as_str())?)
    }
}

/// Request to append an event to a session using typed [`AdkIdentity`] addressing.
///
/// This is the preferred way to append events in new code because it uses the
/// full `(app_name, user_id, session_id)` triple, eliminating ambiguity that
/// can arise when a bare `session_id` string is not globally unique.
///
/// # Example
///
/// ```rust
/// use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
/// use adk_session::AppendEventRequest;
/// use adk_session::Event;
///
/// let identity = AdkIdentity::new(
///     AppName::try_from("weather-app").unwrap(),
///     UserId::try_from("user-123").unwrap(),
///     SessionId::try_from("session-456").unwrap(),
/// );
///
/// let event = Event::new("inv-001");
/// let req = AppendEventRequest { identity, event };
/// ```
#[derive(Debug, Clone)]
pub struct AppendEventRequest {
    /// The typed session-scoped identity triple.
    pub identity: AdkIdentity,
    /// The event to append.
    pub event: Event,
}

/// Request to delete a session.
#[derive(Debug, Clone)]
pub struct DeleteRequest {
    /// Application name that owns the session.
    pub app_name: String,
    /// User identifier for the session owner.
    pub user_id: String,
    /// Session identifier to delete.
    pub session_id: String,
}

impl DeleteRequest {
    /// Returns the stable session-scoped [`AdkIdentity`] triple.
    ///
    /// Parses `app_name`, `user_id`, and `session_id` into their typed
    /// equivalents and combines them into an [`AdkIdentity`].
    ///
    /// # Errors
    ///
    /// Returns an error if any of the three identifiers fail validation.
    pub fn try_identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity {
            app_name: AppName::try_from(self.app_name.as_str())?,
            user_id: UserId::try_from(self.user_id.as_str())?,
            session_id: SessionId::try_from(self.session_id.as_str())?,
        })
    }
}

/// Trait for session persistence backends.
///
/// Implementations manage the full lifecycle of sessions: creation, retrieval,
/// listing, deletion, and event appending.
///
/// Every method validates the app names, user IDs, and session IDs it receives against the
/// [`AppName`], [`UserId`], and [`SessionId`] rules (non-empty, at most
/// [`MAX_ID_LEN`](adk_core::identity::MAX_ID_LEN) bytes, no NUL) before it reads or writes
/// storage, and rejects an invalid one with an
/// [`InvalidInput`](adk_core::ErrorCategory::InvalidInput) error. The built-in backends all
/// follow this rule.
#[async_trait]
pub trait SessionService: Send + Sync {
    /// Creates a new session and returns it.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when an identifier fails validation, and an
    /// [`AdkError`] with code `session.already_exists` and a
    /// [`Conflict`](adk_core::ErrorCategory::Conflict) category when a session with the same
    /// `(app_name, user_id, session_id)` already exists. A conflicting create changes nothing:
    /// the existing session keeps its events and state, and the app and user tiers are not
    /// written. Callers that get-or-create read the session again on a conflict.
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    /// Retrieves an existing session by its complete identity.
    ///
    /// Implementations address sessions by the full `(app_name, user_id, session_id)` tuple.
    /// When that valid identity has no matching record, implementations return an
    /// [`AdkError`] with code `session.not_found` and a
    /// [`NotFound`](adk_core::ErrorCategory::NotFound) category. Backend and transport failures
    /// must not be reported as not-found errors.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when an identifier fails validation, `session.not_found`
    /// when no matching session exists, or a backend-specific error when retrieval fails.
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    /// Lists the sessions of an app and user, most recently updated first.
    ///
    /// Sessions updated at the same instant are ordered by session ID, descending, so
    /// `offset`/`limit` pages are stable.
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    /// Delete a session by its identifiers.
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    /// Appends an event to the session with this ID.
    ///
    /// Events read back in timestamp order; events with equal timestamps keep the order they
    /// were appended in.
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` ([`NotFound`](adk_core::ErrorCategory::NotFound)) when no
    /// session has this ID. An ID alone does not name one session when more than one app or
    /// user holds it, so prefer
    /// [`append_event_for_identity`](SessionService::append_event_for_identity).
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()>;

    /// Get a session using typed [`AdkIdentity`] addressing.
    ///
    /// This is the preferred path for new code. It constructs a [`GetRequest`]
    /// from the full `(app_name, user_id, session_id)` triple so that session
    /// lookup is unambiguous.
    ///
    /// The default implementation delegates to
    /// [`get`](SessionService::get) with a freshly built [`GetRequest`].
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be retrieved.
    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        self.get(GetRequest {
            app_name: identity.app_name.as_ref().to_string(),
            user_id: identity.user_id.as_ref().to_string(),
            session_id: identity.session_id.as_ref().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
    }

    /// Delete a session using typed [`AdkIdentity`] addressing.
    ///
    /// This is the preferred path for new code. It constructs a
    /// [`DeleteRequest`] from the full `(app_name, user_id, session_id)` triple
    /// so that session deletion is unambiguous.
    ///
    /// The default implementation delegates to
    /// [`delete`](SessionService::delete) with a freshly built
    /// [`DeleteRequest`].
    ///
    /// # Errors
    ///
    /// Returns an error if the session cannot be deleted.
    async fn delete_for_identity(&self, identity: &AdkIdentity) -> Result<()> {
        self.delete(DeleteRequest {
            app_name: identity.app_name.as_ref().to_string(),
            user_id: identity.user_id.as_ref().to_string(),
            session_id: identity.session_id.as_ref().to_string(),
        })
        .await
    }

    /// Append an event to a session using typed [`AdkIdentity`] addressing.
    ///
    /// This is the preferred path for new code. It uses the full
    /// `(app_name, user_id, session_id)` triple so that session lookup is
    /// unambiguous even when the same `session_id` string appears under
    /// different apps or users.
    ///
    /// The default implementation delegates to the legacy
    /// [`append_event`](SessionService::append_event) method using only the
    /// `session_id` component. Backends that support composite-key addressing
    /// should override this method to use all three identity fields.
    ///
    /// # Errors
    ///
    /// Returns an error if the event cannot be appended.
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        self.append_event(req.identity.session_id.as_ref(), req.event).await
    }

    /// Erases what the user `user_id` holds in the app `app_name`: every session with its
    /// events, and the user's `user:` state tier.
    ///
    /// The `app:` tier, the user's sessions and state in other apps, and other users'
    /// sessions and state are untouched. No other method removes the `user:` tier, so this is
    /// the call for bulk cleanup and for a request to erase a user's session data. The tier is
    /// erased when the user has no sessions left, and a repeated call succeeds.
    ///
    /// The default implementation validates the identifiers, then returns
    /// `session.delete_all.unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when an identifier fails validation,
    /// `session.delete_all.unsupported` ([`Unsupported`](adk_core::ErrorCategory::Unsupported))
    /// from a backend that does not implement it, or a backend error. A backend that cannot
    /// erase in one transaction may have removed some sessions before the error; calling again
    /// finishes the erasure.
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        validate_scope(app_name, user_id)?;
        Err(AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Unsupported,
            "session.delete_all.unsupported",
            "this session backend does not implement delete_all_sessions; delete each session \
             from `list` and erase the user's `user:` state in the backend",
        ))
    }

    /// Rewinds the session `identity` to `target_event_id`.
    ///
    /// Removes every event after the target and rebuilds the session-scoped state from the
    /// state the session was created with plus the state deltas of the events that remain.
    /// App and user state are shared with other sessions and are not rewound. Only the session
    /// that `identity` names changes, even when other apps or users hold a session with the
    /// same ID.
    ///
    /// # Errors
    ///
    /// Returns `session.rewind.unsupported` ([`Unsupported`](adk_core::ErrorCategory::Unsupported))
    /// when the backend does not support rewind, `session.not_found` when the session does not
    /// exist, and `session.event_not_found` when the target is not an event of the session.
    async fn rewind(
        &self,
        _identity: &AdkIdentity,
        _target_event_id: &str,
    ) -> Result<Box<dyn Session>> {
        Err(rewind_unsupported())
    }

    /// Rewinds the session `identity` by `steps` events from the end.
    ///
    /// Rebuilds state as [`rewind`](SessionService::rewind) does. `steps == 0` returns the
    /// session unchanged, and `steps` equal to the event count removes every event and leaves
    /// the state the session was created with.
    ///
    /// # Errors
    ///
    /// Returns `session.rewind.unsupported` when the backend does not support rewind,
    /// `session.not_found` when the session does not exist, and
    /// `session.rewind.too_many_steps` ([`InvalidInput`](adk_core::ErrorCategory::InvalidInput))
    /// when `steps` exceeds the event count.
    async fn rewind_steps(
        &self,
        _identity: &AdkIdentity,
        _steps: usize,
    ) -> Result<Box<dyn Session>> {
        Err(rewind_unsupported())
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// Use this for Kubernetes readiness probes and `/healthz` endpoints.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
