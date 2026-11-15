use crate::{Events, State};
use adk_core::Result;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use chrono::{DateTime, Utc};

/// Trait representing a conversation session with state and event history.
pub trait Session: Send + Sync {
    /// Returns the session identifier.
    fn id(&self) -> &str;
    /// Returns the application name that owns this session.
    fn app_name(&self) -> &str;
    /// Returns the user identifier for the session owner.
    fn user_id(&self) -> &str;
    /// Returns a reference to the session's state store.
    fn state(&self) -> &dyn State;
    /// Returns a reference to the session's event history.
    fn events(&self) -> &dyn Events;
    /// Returns the timestamp of the last update to this session.
    fn last_update_time(&self) -> DateTime<Utc>;

    /// Moves the event history out of this session, leaving it empty.
    ///
    /// The runner calls it on the session it loaded for an invocation, which no one else
    /// holds, so each turn moves the history instead of copying it. The default returns a
    /// copy of [`events()`](Self::events) and leaves the session unchanged; every
    /// `adk-session` backend moves its events instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{CreateRequest, InMemorySessionService, SessionService};
    ///
    /// # #[tokio::main]
    /// # async fn main() -> adk_core::Result<()> {
    /// let service = InMemorySessionService::new();
    /// let mut session = service
    ///     .create(CreateRequest {
    ///         app_name: "app".into(),
    ///         user_id: "user".into(),
    ///         session_id: None,
    ///         state: Default::default(),
    ///     })
    ///     .await?;
    /// let events = session.take_events();
    /// assert!(events.is_empty());
    /// assert_eq!(session.events().len(), 0);
    /// # Ok(())
    /// # }
    /// ```
    fn take_events(&mut self) -> Vec<adk_core::Event> {
        self.events().all()
    }

    /// Returns the application name as a typed [`AppName`].
    ///
    /// Parses the value returned by [`app_name()`](Self::app_name). Returns an
    /// error if the raw string fails validation (empty, null bytes, or exceeds
    /// the maximum length).
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::config`](adk_core::AdkError::config) when the
    /// underlying string is not a valid identifier.
    fn try_app_name(&self) -> Result<AppName> {
        Ok(AppName::try_from(self.app_name())?)
    }

    /// Returns the user identifier as a typed [`UserId`].
    ///
    /// Parses the value returned by [`user_id()`](Self::user_id). Returns an
    /// error if the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::config`](adk_core::AdkError::config) when the
    /// underlying string is not a valid identifier.
    fn try_user_id(&self) -> Result<UserId> {
        Ok(UserId::try_from(self.user_id())?)
    }

    /// Returns the session identifier as a typed [`SessionId`].
    ///
    /// Parses the value returned by [`id()`](Self::id). Returns an error if
    /// the raw string fails validation.
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::config`](adk_core::AdkError::config) when the
    /// underlying string is not a valid identifier.
    fn try_session_id(&self) -> Result<SessionId> {
        Ok(SessionId::try_from(self.id())?)
    }

    /// Returns the stable session-scoped [`AdkIdentity`] triple.
    ///
    /// Combines [`try_app_name()`](Self::try_app_name),
    /// [`try_user_id()`](Self::try_user_id), and
    /// [`try_session_id()`](Self::try_session_id) into a single composite
    /// identity value.
    ///
    /// # Errors
    ///
    /// Returns an error if any of the three constituent identifiers fail
    /// validation.
    fn try_identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity {
            app_name: self.try_app_name()?,
            user_id: self.try_user_id()?,
            session_id: self.try_session_id()?,
        })
    }
}

/// Key prefix for application-scoped state entries.
pub const KEY_PREFIX_APP: &str = "app:";
/// Key prefix for temporary state entries (stripped on event append).
pub const KEY_PREFIX_TEMP: &str = "temp:";
/// Key prefix for user-scoped state entries.
pub const KEY_PREFIX_USER: &str = "user:";
