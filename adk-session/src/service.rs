use crate::history::{
    ForkRequest, HistoryOutcome, HistoryRewrite, ImportSessionRequest, RewriteHistoryRequest,
    SessionHistory,
};
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

/// Builds the error a backend returns for a history operation it cannot perform.
pub(crate) fn history_unsupported(operation: &str, reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unsupported,
        "session.history.unsupported",
        format!("this session backend cannot {operation}: {reason}"),
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
    /// `offset`/`limit` pages are stable. Archived sessions are listed too, with their
    /// [`ArchiveMark`](crate::ArchiveMark) in their state;
    /// [`list_active`](SessionService::list_active) leaves them out.
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

    /// Reads the stored history of the session `identity`: its events, its own state, and the
    /// state it was created with.
    ///
    /// The default reads the session with [`get_for_identity`](SessionService::get_for_identity)
    /// and reports no recorded creation state; every `adk-session` backend that records one
    /// returns it.
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` when the session does not exist, or a backend error.
    async fn history(&self, identity: &AdkIdentity) -> Result<SessionHistory> {
        let session = self.get_for_identity(identity).await?;
        let (_, _, state) = crate::state_utils::extract_state_deltas(&session.state().all());
        Ok(SessionHistory::new(session.events().all(), state, None))
    }

    /// Rewrites the history of a session in place: a rewind or an edit.
    ///
    /// The backend reads the session, applies [`SessionHistory::rewrite`] to it, and writes the
    /// result atomically, so a concurrent append either lands before the read or after the
    /// write. The session's own state is rebuilt from the state it was created with plus the
    /// state deltas of the events it keeps; the `app:` and `user:` tiers do not change. Only the
    /// session that `identity` names changes, even when other apps or users hold a session with
    /// the same ID. The rules every history operation follows are in [`crate::history`].
    ///
    /// # Errors
    ///
    /// Returns `session.history.unsupported` ([`Unsupported`](adk_core::ErrorCategory::Unsupported))
    /// when the backend cannot delete events, `session.not_found` when the session does not
    /// exist, and the errors of [`SessionHistory::rewrite`].
    async fn rewrite_history(&self, _req: RewriteHistoryRequest) -> Result<HistoryOutcome> {
        Err(history_unsupported("rewrite history", "it does not implement rewrite_history"))
    }

    /// Creates a session that already holds events, without applying their state deltas to
    /// the `app:` and `user:` tiers.
    ///
    /// # Errors
    ///
    /// Returns `session.history.unsupported` when the backend does not implement it,
    /// `session.already_exists` ([`Conflict`](adk_core::ErrorCategory::Conflict)) when the session
    /// exists, and an invalid-input error when an identifier fails validation.
    async fn import_session(&self, _req: ImportSessionRequest) -> Result<Box<dyn Session>> {
        Err(history_unsupported("import a session", "it does not implement import_session"))
    }

    /// Forks a session: creates a session of the same app and user that holds the source's
    /// events up to [`ForkRequest::at_event_id`], with the state the source had there.
    ///
    /// The source does not change. The fork's events keep their IDs and timestamps, its state is
    /// the source's creation state plus the deltas of the copied events, and it records its
    /// [`ForkOrigin`](crate::ForkOrigin) under [`FORK_ORIGIN_KEY`](crate::FORK_ORIGIN_KEY). The
    /// default reads the source with [`history`](SessionService::history) and creates the fork
    /// with [`import_session`](SessionService::import_session).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{CreateRequest, Event, ForkRequest, InMemorySessionService, SessionService};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_core::Result<()> {
    /// let service = InMemorySessionService::new();
    /// let source = service
    ///     .create(CreateRequest {
    ///         app_name: "app".into(),
    ///         user_id: "alice".into(),
    ///         session_id: Some("main".into()),
    ///         state: Default::default(),
    ///     })
    ///     .await?;
    /// let identity = source.try_identity()?;
    /// let first = Event::new("inv-1");
    /// let first_id = first.id.clone();
    /// for event in [first, Event::new("inv-2")] {
    ///     service
    ///         .append_event_for_identity(adk_session::AppendEventRequest {
    ///             identity: identity.clone(),
    ///             event,
    ///         })
    ///         .await?;
    /// }
    ///
    /// let fork = service.fork(ForkRequest::new(identity).at_event(first_id)).await?;
    /// assert_eq!(fork.session.events().len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` when the source does not exist, `session.already_exists`
    /// when the fork's session ID is taken, `session.history.unsupported` from a backend that
    /// cannot import, and the errors of [`SessionHistory::fork`].
    async fn fork(&self, req: ForkRequest) -> Result<HistoryOutcome> {
        let source = self.history(&req.source).await?;
        let (import, change) = source.fork(&req)?;
        let session = self.import_session(import).await?;
        Ok(HistoryOutcome::new(session, change))
    }

    /// Rewinds the session `identity` to `target_event_id`, removing every event after it.
    ///
    /// A [`rewrite_history`](SessionService::rewrite_history) with
    /// [`HistoryRewrite::RewindTo`].
    ///
    /// # Errors
    ///
    /// Returns `session.event_not_found` when the target is not an event of the session, and
    /// the errors of [`rewrite_history`](SessionService::rewrite_history).
    async fn rewind(
        &self,
        identity: &AdkIdentity,
        target_event_id: &str,
    ) -> Result<Box<dyn Session>> {
        let rewrite = HistoryRewrite::RewindTo { event_id: target_event_id.to_string() };
        Ok(self
            .rewrite_history(RewriteHistoryRequest::new(identity.clone(), rewrite))
            .await?
            .session)
    }

    /// Rewinds the session `identity` by `steps` events from the end.
    ///
    /// `steps == 0` returns the session unchanged, and `steps` equal to the event count removes
    /// every event and leaves the state the session was created with.
    ///
    /// # Errors
    ///
    /// Returns `session.rewind.too_many_steps` ([`InvalidInput`](adk_core::ErrorCategory::InvalidInput))
    /// when `steps` exceeds the event count, and the errors of
    /// [`rewrite_history`](SessionService::rewrite_history).
    async fn rewind_steps(&self, identity: &AdkIdentity, steps: usize) -> Result<Box<dyn Session>> {
        if steps == 0 {
            return self.get_for_identity(identity).await;
        }
        let rewrite = HistoryRewrite::RewindSteps { steps };
        Ok(self
            .rewrite_history(RewriteHistoryRequest::new(identity.clone(), rewrite))
            .await?
            .session)
    }

    /// Verify backend connectivity.
    ///
    /// Returns `Ok(())` if the backend is reachable and responsive.
    /// Use this for Kubernetes readiness probes and `/healthz` endpoints.
    /// The default implementation always succeeds (suitable for in-memory).
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// Archives the session `identity`: it keeps its events and state, is left out of
    /// [`list_active`](SessionService::list_active), and takes no new turns until
    /// [`unarchive`](SessionService::unarchive) (see [`crate::archive`]).
    ///
    /// The mark is written to the session's own state under [`ARCHIVE_KEY`](crate::ARCHIVE_KEY)
    /// by a content-less marker event, through
    /// [`append_event_for_identity`](SessionService::append_event_for_identity), so it works on
    /// every backend. Archiving an archived session changes nothing and returns its mark.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_session::{CreateRequest, InMemorySessionService, SessionService};
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
    /// let first = service.archive(&identity, Some("done".into())).await?;
    /// let again = service.archive(&identity, None).await?;
    /// assert_eq!(first, again);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` when the session does not exist, or a backend error.
    async fn archive(
        &self,
        identity: &AdkIdentity,
        reason: Option<String>,
    ) -> Result<crate::ArchiveMark> {
        let session = self.get_for_identity(identity).await?;
        if let Some(mark) = crate::ArchiveMark::of(session.as_ref()) {
            return Ok(mark);
        }
        let mark = crate::ArchiveMark::new(reason);
        self.append_event_for_identity(AppendEventRequest {
            identity: identity.clone(),
            event: mark.event()?,
        })
        .await?;
        Ok(mark)
    }

    /// Unarchives the session `identity`: it lists and takes turns again. Unarchiving a
    /// session that is not archived changes nothing.
    ///
    /// The mark is cleared by a content-less marker event that sets
    /// [`ARCHIVE_KEY`](crate::ARCHIVE_KEY) to `null`.
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` when the session does not exist, or a backend error.
    async fn unarchive(&self, identity: &AdkIdentity) -> Result<()> {
        let session = self.get_for_identity(identity).await?;
        if crate::ArchiveMark::of(session.as_ref()).is_none() {
            return Ok(());
        }
        self.append_event_for_identity(AppendEventRequest {
            identity: identity.clone(),
            event: crate::archive::marker_event(Value::Null),
        })
        .await
    }

    /// Writes the session `identity` out as a portable [`SessionBundle`](crate::SessionBundle):
    /// its identity, its own state, every event and its archive mark.
    ///
    /// # Errors
    ///
    /// Returns `session.not_found` when the session does not exist, or a backend error.
    async fn export_session(&self, identity: &AdkIdentity) -> Result<crate::SessionBundle> {
        let session = self.get_for_identity(identity).await?;
        Ok(crate::SessionBundle::of(session.as_ref()))
    }

    /// Lists the sessions of an app and user that are not archived, in
    /// [`list`](SessionService::list)'s order; `offset` and `limit` count only these.
    ///
    /// This is the listing a person sees: `adk-rust sessions list` and the REST API's
    /// session list use it. The default reads [`list`](SessionService::list) a page at a
    /// time and leaves archived sessions out.
    ///
    /// # Errors
    ///
    /// Returns what [`list`](SessionService::list) returns.
    async fn list_active(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        list_kept(self, req, crate::archive::Keep::Active).await
    }

    /// Lists the archived sessions of an app and user, in [`list`](SessionService::list)'s
    /// order; `offset` and `limit` count only these.
    ///
    /// # Errors
    ///
    /// Returns what [`list`](SessionService::list) returns.
    async fn list_archived(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        list_kept(self, req, crate::archive::Keep::Archived).await
    }
}

/// The sessions of `req` that `keep` keeps, with `offset` and `limit` counting only those,
/// read from `list` a page at a time.
async fn list_kept<S: SessionService + ?Sized>(
    service: &S,
    req: ListRequest,
    keep: crate::archive::Keep,
) -> Result<Vec<Box<dyn Session>>> {
    validate_scope(&req.app_name, &req.user_id)?;
    let mut skip = req.offset.unwrap_or(0);
    let limit = req.limit.unwrap_or(usize::MAX);
    let mut kept = Vec::new();
    let mut position = 0;
    while kept.len() < limit {
        let page = service
            .list(ListRequest {
                app_name: req.app_name.clone(),
                user_id: req.user_id.clone(),
                limit: Some(crate::archive::LIST_PAGE),
                offset: Some(position),
            })
            .await?;
        let read = page.len();
        position += read;
        for session in page {
            if !keep.keeps(session.as_ref()) {
                continue;
            }
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if kept.len() < limit {
                kept.push(session);
            }
        }
        if read < crate::archive::LIST_PAGE {
            break;
        }
    }
    Ok(kept)
}
