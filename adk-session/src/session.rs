use crate::{Events, State};
use adk_core::Result;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use chrono::{DateTime, Utc};

pub trait Session: Send + Sync {
    fn id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn state(&self) -> &dyn State;
    fn events(&self) -> &dyn Events;
    fn last_update_time(&self) -> DateTime<Utc>;

    /// Returns the application name as a typed [`AppName`].
    ///
    /// Parses the value returned by [`app_name()`](Self::app_name). Returns an
    /// error if the raw string fails validation (empty, null bytes, or exceeds
    /// the maximum length).
    ///
    /// # Errors
    ///
    /// Returns [`AdkError::Config`](adk_core::AdkError::Config) when the
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
    /// Returns [`AdkError::Config`](adk_core::AdkError::Config) when the
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
    /// Returns [`AdkError::Config`](adk_core::AdkError::Config) when the
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

pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_TEMP: &str = "temp:";
pub const KEY_PREFIX_USER: &str = "user:";
