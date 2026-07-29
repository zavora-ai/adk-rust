use crate::{Event, Session};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorComponent, Result};
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
    ///
    /// # Errors
    ///
    /// Returns an error if any of the constituent identifiers fail validation.
    pub fn try_identity(&self) -> Result<Option<AdkIdentity>> {
        let Some(sid) = self.try_session_id()? else {
            return Ok(None);
        };
        Ok(Some(AdkIdentity {
            app_name: self.try_app_name()?,
            user_id: self.try_user_id()?,
            session_id: sid,
        }))
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

/// Builds the canonical error returned when a session identity has no matching record.
pub(crate) fn session_not_found(req: &GetRequest) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Session,
        "session.not_found",
        format!(
            "session '{}' was not found for app '{}' and user '{}'",
            req.session_id, req.app_name, req.user_id
        ),
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
#[async_trait]
pub trait SessionService: Send + Sync {
    /// Create a new session and return it.
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
    /// List sessions for a given app and user.
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    /// Delete a session by its identifiers.
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    /// Append an event to a session identified by its session ID string.
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

    /// Delete all sessions for a given app and user.
    ///
    /// Removes all sessions and their associated events. Useful for
    /// bulk cleanup and GDPR right-to-erasure compliance.
    /// The default implementation returns an error.
    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        let _ = (app_name, user_id);
        Err(adk_core::AdkError::session("delete_all_sessions not implemented"))
    }

    /// Rewind a session to the specified event, removing all subsequent events
    /// and rebuilding state from remaining events' state deltas.
    ///
    /// After rewinding, the session will contain only events up to and including
    /// the target event, and the session state will reflect the cumulative
    /// application of those events' state deltas.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend does not support rewind, the session is
    /// not found, or the target event ID does not exist in the session.
    async fn rewind(&self, _session_id: &str, _target_event_id: &str) -> Result<Box<dyn Session>> {
        Err(adk_core::AdkError::session("rewind not supported by this backend"))
    }

    /// Rewind a session by N steps from the end.
    ///
    /// If `steps` is 0, returns the session unchanged. If `steps` exceeds the
    /// number of events, returns an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend does not support rewind, the session is
    /// not found, or `steps` exceeds the event count.
    async fn rewind_steps(&self, _session_id: &str, _steps: usize) -> Result<Box<dyn Session>> {
        Err(adk_core::AdkError::session("rewind_steps not supported by this backend"))
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
