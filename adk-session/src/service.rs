use crate::{Event, Session};
use adk_core::Result;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct CreateRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: Option<String>,
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

#[derive(Debug, Clone)]
pub struct GetRequest {
    pub app_name: String,
    pub user_id: String,
    pub session_id: String,
    pub num_recent_events: Option<usize>,
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

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub app_name: String,
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

#[derive(Debug, Clone)]
pub struct DeleteRequest {
    pub app_name: String,
    pub user_id: String,
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

#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
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

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// Use this for Kubernetes readiness probes and `/healthz` endpoints.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
