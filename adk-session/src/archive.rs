//! Session archive: put a finished conversation away without deleting it.
//!
//! An archived session keeps its events and state, is left out of
//! [`list_active`](crate::SessionService::list_active), takes no new turns, and comes back with
//! [`unarchive`](crate::SessionService::unarchive). [`export_session`](crate::SessionService::export_session)
//! writes any session, archived or not, as a portable [`SessionBundle`].
//!
//! | Rule | Where it is kept |
//! |---|---|
//! | The mark lives with the session | Its own state, under [`ARCHIVE_KEY`], written by a content-less marker event, so every backend that keeps session state keeps it and the archive is in the session's history |
//! | Hidden from default lists | [`list_active`](crate::SessionService::list_active) leaves archived sessions out; offset and limit count only the sessions it returns. [`list`](crate::SessionService::list) still lists every session, archived ones with their mark |
//! | Read-only | [`ensure_writable`] refuses an archived session with `session.archived`; `adk_runner::Runner` calls it before every turn and before recovering held calls, so no agent, schedule, chat or API turn adds to it |
//! | Restorable | [`unarchive`](crate::SessionService::unarchive) clears the mark with another marker event; the session lists and takes turns again |
//! | Portable | A [`SessionBundle`] is JSON (`adk.session.bundle/v1`): identity, the session's own state, every event, the archive mark |
//!
//! Marker events carry no content, so no model ever sees them.
//!
//! # Example
//!
//! ```rust
//! use adk_session::{CreateRequest, InMemorySessionService, ListRequest, SessionService};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_core::Result<()> {
//! let service = InMemorySessionService::new();
//! let session = service
//!     .create(CreateRequest {
//!         app_name: "app".into(),
//!         user_id: "ana".into(),
//!         session_id: Some("trip".into()),
//!         state: Default::default(),
//!     })
//!     .await?;
//! let identity = session.try_identity()?;
//!
//! let mark = service.archive(&identity, Some("trip booked".into())).await?;
//! assert_eq!(mark.reason.as_deref(), Some("trip booked"));
//! let listed = ListRequest { app_name: "app".into(), user_id: "ana".into(), limit: None, offset: None };
//! assert!(service.list_active(listed.clone()).await?.is_empty());
//! assert_eq!(service.list_archived(listed.clone()).await?.len(), 1);
//!
//! let bundle = service.export_session(&identity).await?;
//! assert_eq!(bundle.archived, Some(mark));
//!
//! service.unarchive(&identity).await?;
//! assert_eq!(service.list_active(listed).await?.len(), 1);
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;

use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Event, Session};

/// The session-state key an archived session's [`ArchiveMark`] is kept under.
pub const ARCHIVE_KEY: &str = "adk:archived";

/// The author of the marker events [`archive`](crate::SessionService::archive) and
/// [`unarchive`](crate::SessionService::unarchive) append.
pub const ARCHIVE_AUTHOR: &str = "adk.session";

/// The format a [`SessionBundle`] declares, so a reader knows what it holds.
pub const SESSION_BUNDLE_FORMAT: &str = "adk.session.bundle/v1";

/// How many sessions [`list_active`](crate::SessionService::list_active) and
/// [`list_archived`](crate::SessionService::list_archived) read from
/// [`list`](crate::SessionService::list) at a time.
pub(crate) const LIST_PAGE: usize = 100;

/// When and why a session was archived, kept in its state under [`ARCHIVE_KEY`].
///
/// # Example
///
/// ```rust
/// use adk_session::ArchiveMark;
///
/// let mark = ArchiveMark::new(Some("done".into()));
/// let json = serde_json::to_value(&mark).unwrap();
/// assert_eq!(json["reason"], "done");
/// assert!(json["archivedAt"].is_string());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ArchiveMark {
    /// When the session was archived.
    pub archived_at: DateTime<Utc>,
    /// Why, in the words of whoever archived it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ArchiveMark {
    /// A mark made now, with `reason`.
    pub fn new(reason: Option<String>) -> Self {
        Self { archived_at: Utc::now(), reason }
    }

    /// The mark `session` carries, or `None` when it is not archived.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{ArchiveMark, CreateRequest, InMemorySessionService, SessionService};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_core::Result<()> {
    /// let service = InMemorySessionService::new();
    /// let session = service
    ///     .create(CreateRequest {
    ///         app_name: "app".into(),
    ///         user_id: "ana".into(),
    ///         session_id: None,
    ///         state: Default::default(),
    ///     })
    ///     .await?;
    /// assert_eq!(ArchiveMark::of(session.as_ref()), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn of(session: &dyn Session) -> Option<Self> {
        Self::in_state(session.state().get(ARCHIVE_KEY).as_ref())
    }

    /// The mark a state value holds: an object is a mark, anything else (`null` once
    /// unarchived) is not.
    fn in_state(value: Option<&Value>) -> Option<Self> {
        value
            .filter(|value| value.is_object())
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// The marker event that writes this mark into a session's state.
    pub(crate) fn event(&self) -> Result<Event> {
        let value = serde_json::to_value(self).map_err(|error| {
            AdkError::session(format!("the archive mark could not be serialized: {error}"))
        })?;
        Ok(marker_event(value))
    }
}

/// The content-less event that sets [`ARCHIVE_KEY`] to `value`.
pub(crate) fn marker_event(value: Value) -> Event {
    let mut event = Event::new(format!("archive-{}", uuid::Uuid::new_v4()));
    event.author = ARCHIVE_AUTHOR.to_string();
    event.actions.state_delta.insert(ARCHIVE_KEY.to_string(), value);
    event
}

/// Whether `event` is a marker that sets or clears [`ARCHIVE_KEY`].
pub(crate) fn is_marker_event(event: &Event) -> bool {
    event.author == ARCHIVE_AUTHOR && event.actions.state_delta.contains_key(ARCHIVE_KEY)
}

/// Refuses an archived session with `session.archived`, so nothing is added to it.
///
/// `adk_runner::Runner` calls this on the session it loaded before every turn. A host that
/// appends events to a session itself calls it the same way.
///
/// # Errors
///
/// Returns `session.archived` ([`Conflict`](adk_core::ErrorCategory::Conflict)) when the session
/// carries an [`ArchiveMark`].
///
/// # Example
///
/// ```rust
/// use adk_session::{CreateRequest, InMemorySessionService, SessionService, ensure_writable};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = InMemorySessionService::new();
/// let session = service
///     .create(CreateRequest {
///         app_name: "app".into(),
///         user_id: "ana".into(),
///         session_id: Some("s".into()),
///         state: Default::default(),
///     })
///     .await?;
/// let identity = session.try_identity()?;
/// ensure_writable(session.as_ref())?;
///
/// service.archive(&identity, None).await?;
/// let archived = service.get_for_identity(&identity).await?;
/// let error = ensure_writable(archived.as_ref()).unwrap_err();
/// assert_eq!(error.code, "session.archived");
/// # Ok(())
/// # }
/// ```
pub fn ensure_writable(session: &dyn Session) -> Result<()> {
    match ArchiveMark::of(session) {
        None => Ok(()),
        Some(mark) => Err(session_archived(session.id(), &mark)),
    }
}

/// The error for a change to an archived session.
pub(crate) fn session_archived(session_id: &str, mark: &ArchiveMark) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Conflict,
        "session.archived",
        format!(
            "session '{session_id}' was archived at {} and is read-only; unarchive it with \
             SessionService::unarchive (`adk-rust sessions unarchive {session_id}`) to continue \
             it",
            mark.archived_at.to_rfc3339()
        ),
    )
}

/// A session written out as one portable JSON document (`adk.session.bundle/v1`).
///
/// It holds the session's identity, its own state (the `app:` and `user:` tiers belong to
/// other sessions too and stay out), every event in order, and the archive mark. Any tool that
/// reads JSON reads it; the events are [`adk_core::Event`] as every backend stores them.
///
/// # Example
///
/// ```rust
/// use adk_session::{CreateRequest, InMemorySessionService, SessionBundle, SessionService};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let service = InMemorySessionService::new();
/// let session = service
///     .create(CreateRequest {
///         app_name: "app".into(),
///         user_id: "ana".into(),
///         session_id: Some("s".into()),
///         state: [("topic".to_string(), serde_json::json!("trip"))].into(),
///     })
///     .await?;
/// let bundle = service.export_session(&session.try_identity()?).await?;
/// let json = serde_json::to_string(&bundle).unwrap();
/// let read: SessionBundle = serde_json::from_str(&json).unwrap();
/// assert_eq!(read.format, "adk.session.bundle/v1");
/// assert_eq!(read.state["topic"], "trip");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SessionBundle {
    /// Always [`SESSION_BUNDLE_FORMAT`].
    pub format: String,
    /// When the bundle was written.
    pub exported_at: DateTime<Utc>,
    /// The application the session belongs to.
    pub app_name: String,
    /// The user the session belongs to.
    pub user_id: String,
    /// The session's ID.
    pub session_id: String,
    /// When the session last changed.
    pub updated_at: DateTime<Utc>,
    /// The session's own state, the archive mark included.
    pub state: HashMap<String, Value>,
    /// The archive mark, when the session is archived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<ArchiveMark>,
    /// Every event, oldest first.
    pub events: Vec<Event>,
}

impl SessionBundle {
    /// The bundle of `session`, which holds its full history.
    pub fn of(session: &dyn Session) -> Self {
        let (_, _, state) = crate::state_utils::extract_state_deltas(&session.state().all());
        Self {
            format: SESSION_BUNDLE_FORMAT.to_string(),
            exported_at: Utc::now(),
            app_name: session.app_name().to_string(),
            user_id: session.user_id().to_string(),
            session_id: session.id().to_string(),
            updated_at: session.last_update_time(),
            state,
            archived: ArchiveMark::of(session),
            events: session.events().all(),
        }
    }

    /// The identity of the session the bundle holds.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when an identifier fails validation.
    pub fn identity(&self) -> Result<AdkIdentity> {
        Ok(AdkIdentity::new(
            self.app_name.as_str().try_into()?,
            self.user_id.as_str().try_into()?,
            self.session_id.as_str().try_into()?,
        ))
    }
}

/// Which sessions a listing keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    Active,
    Archived,
}

impl Keep {
    pub(crate) fn keeps(self, session: &dyn Session) -> bool {
        let archived = ArchiveMark::of(session).is_some();
        match self {
            Self::Active => !archived,
            Self::Archived => archived,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CreateRequest, InMemorySessionService, ListRequest, SessionService};

    fn listing(limit: Option<usize>, offset: Option<usize>) -> ListRequest {
        ListRequest { app_name: "app".into(), user_id: "ana".into(), limit, offset }
    }

    fn ids(sessions: &[Box<dyn Session>]) -> Vec<String> {
        sessions.iter().map(|session| session.id().to_string()).collect()
    }

    /// Pages of `list_active` and `list_archived` span several pages of `list` and count only
    /// the sessions they keep.
    #[tokio::test]
    async fn listings_page_across_several_reads_of_list() {
        let service = InMemorySessionService::new();
        for index in 0..(LIST_PAGE * 2 + 50) {
            let session = service
                .create(CreateRequest {
                    app_name: "app".into(),
                    user_id: "ana".into(),
                    session_id: Some(format!("s{index:04}")),
                    state: Default::default(),
                })
                .await
                .unwrap();
            if index % 3 == 0 {
                service.archive(&session.try_identity().unwrap(), None).await.unwrap();
            }
        }
        let all = service.list(listing(None, None)).await.unwrap();
        let active: Vec<String> = all
            .iter()
            .filter(|session| ArchiveMark::of(session.as_ref()).is_none())
            .map(|session| session.id().to_string())
            .collect();
        let archived: Vec<String> = all
            .iter()
            .filter(|session| ArchiveMark::of(session.as_ref()).is_some())
            .map(|session| session.id().to_string())
            .collect();
        assert_eq!((active.len(), archived.len()), (166, 84));

        assert_eq!(ids(&service.list_active(listing(None, None)).await.unwrap()), active);
        assert_eq!(ids(&service.list_archived(listing(None, None)).await.unwrap()), archived);
        let page = service.list_active(listing(Some(100), Some(50))).await.unwrap();
        assert_eq!(ids(&page), active[50..150].to_vec());
        let tail = service.list_active(listing(Some(100), Some(150))).await.unwrap();
        assert_eq!(ids(&tail), active[150..].to_vec());
        let page = service.list_archived(listing(Some(10), Some(80))).await.unwrap();
        assert_eq!(ids(&page), archived[80..].to_vec());
        assert!(service.list_active(listing(Some(5), Some(500))).await.unwrap().is_empty());
    }

    /// A state value other than an object, such as the `null` an unarchive writes, is no mark.
    #[test]
    fn only_an_object_is_a_mark() {
        assert_eq!(ArchiveMark::in_state(None), None);
        assert_eq!(ArchiveMark::in_state(Some(&Value::Null)), None);
        assert_eq!(ArchiveMark::in_state(Some(&serde_json::json!(true))), None);
        let mark = ArchiveMark::new(Some("done".into()));
        let value = serde_json::to_value(&mark).unwrap();
        assert_eq!(ArchiveMark::in_state(Some(&value)), Some(mark.clone()));
        let event = mark.event().unwrap();
        assert_eq!(event.author, ARCHIVE_AUTHOR);
        assert!(event.llm_response.content.is_none());
        assert_eq!(event.actions.state_delta[ARCHIVE_KEY], value);
        let error = session_archived("s1", &mark);
        assert_eq!((error.code, error.category), ("session.archived", ErrorCategory::Conflict));
        assert!(error.message.contains("adk-rust sessions unarchive s1"), "{}", error.message);
    }
}
