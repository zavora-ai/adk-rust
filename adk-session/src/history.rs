//! Forking a session, rewinding it, and editing its history.
//!
//! | Operation | Method | Effect |
//! |-----------|--------|--------|
//! | Fork | [`SessionService::fork`](crate::SessionService::fork) | A new session of the same app and user holding the source's events up to an event |
//! | Rewind | [`rewind`](crate::SessionService::rewind), [`rewind_steps`](crate::SessionService::rewind_steps) | Removes the events after a target event |
//! | Edit | [`rewrite_history`](crate::SessionService::rewrite_history) with [`HistoryRewrite::Edit`] | Replaces or removes events |
//!
//! Every operation rebuilds the session's own state from the state it was created with plus the
//! state deltas of the events it keeps. The `app:` and `user:` tiers are shared with other
//! sessions and never change: the deltas of removed or replaced events stay in them.
//!
//! Three rules hold on every backend:
//!
//! - **Tool calls stay paired.** An operation that would keep a tool call whose response it
//!   removes, or keep a response whose call it removes, fails with
//!   `session.history.splits_tool_call`; no provider then receives a call without its result.
//! - **Held calls block rewrites.** A rewind or edit of a session that holds a call for a
//!   decision fails with `session.history.held_call`, and a fork that would copy a held call
//!   fails the same way. Decide the call first, or fork at an earlier event.
//! - **Writes are atomic per session.** A rewrite reads, checks, and writes the session in one
//!   transaction or one guarded script; [`RewriteHistoryRequest::expected_version`] refuses a
//!   rewrite when the history changed since the caller read [`SessionHistory::version`].
//!
//! A fork records where it came from in its state under [`FORK_ORIGIN_KEY`]; read it back with
//! [`ForkOrigin::of`]. Each operation returns a [`HistoryChange`] naming what changed, which the
//! runner and server record in the audit log.

use crate::{Event, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER, Session};
use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Part, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

type StateMap = HashMap<String, Value>;

/// The state key under which a fork records its [`ForkOrigin`].
///
/// The key is session-scoped and part of the state the fork was created with, so rewinding the
/// fork keeps it.
pub const FORK_ORIGIN_KEY: &str = "adk:forked_from";

/// Where a forked session came from.
///
/// # Example
///
/// ```rust
/// use adk_core::identity::AdkIdentity;
/// use adk_session::{CreateRequest, ForkOrigin, ForkRequest, InMemorySessionService, SessionService};
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
/// let fork = service.fork(ForkRequest::new(source.try_identity()?)).await?;
/// let origin = ForkOrigin::of(fork.session.as_ref()).expect("a fork records its origin");
/// assert_eq!(origin.session_id, "main");
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ForkOrigin {
    /// The session the fork was taken from.
    pub session_id: String,
    /// The last event the fork copied; `None` when the source held no events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// When the fork was taken.
    pub forked_at: DateTime<Utc>,
}

impl ForkOrigin {
    /// The origin `session` recorded when it was forked, or `None` for a session that is not a
    /// fork.
    pub fn of(session: &dyn Session) -> Option<Self> {
        session.state().get(FORK_ORIGIN_KEY).and_then(|value| serde_json::from_value(value).ok())
    }
}

/// A session's stored history: its events, its own state, and the state it was created with.
///
/// Backends read it inside the transaction of a rewrite; callers read it with
/// [`SessionService::history`](crate::SessionService::history).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SessionHistory {
    /// The events in read order.
    pub events: Vec<Event>,
    /// The session's own state: the session tier on backends that keep `app:` and `user:`
    /// tiers apart, every key on a backend that does not.
    pub state: StateMap,
    /// The session state the session was created with, or `None` for a session created before
    /// its backend recorded it.
    pub initial_state: Option<StateMap>,
    /// Whether `app:` and `user:` keys live in shared tiers rather than in the session.
    pub shared_tiers: bool,
}

impl SessionHistory {
    /// The history of a backend that keeps `app:` and `user:` keys in shared tiers.
    pub fn new(events: Vec<Event>, state: StateMap, initial_state: Option<StateMap>) -> Self {
        Self { events, state, initial_state, shared_tiers: true }
    }

    /// The history of a backend that stores `app:` and `user:` keys in the session itself.
    pub fn without_shared_tiers(
        events: Vec<Event>,
        state: StateMap,
        initial_state: Option<StateMap>,
    ) -> Self {
        Self { events, state, initial_state, shared_tiers: false }
    }

    /// A digest of the events, which changes whenever one is appended, removed, or replaced.
    ///
    /// Pass it as [`RewriteHistoryRequest::expected_version`] to rewrite only the history the
    /// caller read.
    pub fn version(&self) -> String {
        history_version(&self.events)
    }

    /// Whether `key` belongs to this session rather than to a shared tier or to no store.
    fn session_scoped(&self, key: &str) -> bool {
        !key.starts_with(KEY_PREFIX_TEMP)
            && (!self.shared_tiers
                || !(key.starts_with(KEY_PREFIX_APP) || key.starts_with(KEY_PREFIX_USER)))
    }

    /// The session-scoped keys of `state`.
    fn scoped(&self, state: &StateMap) -> StateMap {
        state
            .iter()
            .filter(|(key, _)| self.session_scoped(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    /// The state the session was created with: the recorded one, or for a session created
    /// before its backend recorded it, its current keys that no event wrote, whose values
    /// cannot have changed since it was created.
    pub fn creation_state(&self) -> StateMap {
        if let Some(initial) = &self.initial_state {
            return self.scoped(initial);
        }
        let mut state = self.scoped(&self.state);
        for event in &self.events {
            for key in event.actions.state_delta.keys() {
                state.remove(key);
            }
        }
        state
    }

    /// The session state after `events`: the creation state plus their session-scoped deltas.
    pub fn state_after<'a>(&self, events: impl IntoIterator<Item = &'a Event>) -> StateMap {
        let mut state = self.creation_state();
        for event in events {
            state.extend(self.scoped(&event.actions.state_delta));
        }
        state
    }

    fn position(&self, identity: &AdkIdentity, event_id: &str) -> Result<usize> {
        let mut found = self.events.iter().enumerate().filter(|(_, event)| event.id == event_id);
        match (found.next(), found.next()) {
            (Some((index, _)), None) => Ok(index),
            (Some(_), Some(_)) => Err(history_error(
                ErrorCategory::InvalidInput,
                "session.history.ambiguous_event",
                format!(
                    "event '{event_id}' appears more than once in session '{}'; the history \
                     cannot address it",
                    identity.session_id
                ),
            )),
            (None, _) => Err(event_not_found(identity, event_id)),
        }
    }

    /// Applies `rewrite` to this history and returns what the session holds afterwards.
    ///
    /// Backends call it inside the transaction that writes the result, so the checks and the
    /// write see the same history.
    ///
    /// # Errors
    ///
    /// Returns `session.history.version_mismatch` when `expected_version` no longer matches,
    /// `session.history.held_call` while the session holds a call for a decision,
    /// `session.event_not_found` for a target the session does not hold,
    /// `session.rewind.too_many_steps` for more steps than events, and
    /// `session.history.splits_tool_call` when the result would separate a tool call from its
    /// response.
    pub fn rewrite(
        &self,
        identity: &AdkIdentity,
        rewrite: &HistoryRewrite,
        expected_version: Option<&str>,
    ) -> Result<RewrittenHistory> {
        let version_before = self.version();
        if let Some(expected) = expected_version
            && expected != version_before
        {
            return Err(history_error(
                ErrorCategory::Conflict,
                "session.history.version_mismatch",
                format!(
                    "the history of session '{}' changed since version '{expected}' was read; \
                     read it again and retry",
                    identity.session_id
                ),
            ));
        }
        if !matches!(rewrite, HistoryRewrite::RewindSteps { steps: 0 }) {
            refuse_held_calls(identity, &self.events, "rewritten")?;
        }

        let (events, operation, removed, replaced) = match rewrite {
            HistoryRewrite::RewindTo { event_id } => {
                let keep = self.position(identity, event_id)? + 1;
                let removed = self.events[keep..].iter().map(|e| e.id.clone()).collect();
                (self.events[..keep].to_vec(), HistoryOperation::Rewind, removed, Vec::new())
            }
            HistoryRewrite::RewindSteps { steps } => {
                let keep = self.events.len().checked_sub(*steps).ok_or_else(|| {
                    crate::service::rewind_steps_exceed(identity, *steps, self.events.len())
                })?;
                let removed = self.events[keep..].iter().map(|e| e.id.clone()).collect();
                (self.events[..keep].to_vec(), HistoryOperation::Rewind, removed, Vec::new())
            }
            HistoryRewrite::Edit { edits } => {
                let mut events = self.events.clone();
                let mut removed = Vec::new();
                let mut replaced = Vec::new();
                for edit in edits {
                    match edit {
                        HistoryEdit::Replace { event } => {
                            let index = self.position(identity, &event.id)?;
                            let current = events
                                .iter()
                                .position(|stored| stored.id == event.id)
                                .ok_or_else(|| event_not_found(identity, &event.id))?;
                            let mut replacement = Event::clone(event);
                            // Order is (timestamp, append order), so the original timestamp
                            // keeps the replacement where the original stood.
                            replacement.timestamp = self.events[index].timestamp;
                            replacement
                                .actions
                                .state_delta
                                .retain(|key, _| !key.starts_with(KEY_PREFIX_TEMP));
                            events[current] = replacement;
                            replaced.push(event.id.clone());
                        }
                        HistoryEdit::Remove { event_id } => {
                            self.position(identity, event_id)?;
                            let current = events
                                .iter()
                                .position(|stored| &stored.id == event_id)
                                .ok_or_else(|| event_not_found(identity, event_id))?;
                            events.remove(current);
                            removed.push(event_id.clone());
                        }
                    }
                }
                replaced.retain(|id| !removed.contains(id));
                (events, HistoryOperation::Edit, removed, replaced)
            }
        };

        refuse_split_tool_calls(identity, &self.events, &events)?;
        let state = self.state_after(&events);
        let changed: Vec<&Event> = self
            .events
            .iter()
            .filter(|event| removed.contains(&event.id) || replaced.contains(&event.id))
            .collect();
        let change = HistoryChange {
            operation,
            session_id: identity.session_id.as_ref().to_string(),
            version_before,
            version_after: history_version(&events),
            removed,
            replaced,
            changed_digest: (!changed.is_empty()).then(|| events_digest(&changed)),
            events_after: events.len(),
            fork_origin: None,
        };
        Ok(RewrittenHistory { events, state, change })
    }

    /// Plans the fork `request` of this history: the session to import and the change record.
    ///
    /// # Errors
    ///
    /// Returns `session.event_not_found` for a fork point the session does not hold,
    /// `session.history.held_call` when the copied events hold a call for a decision, and
    /// `session.history.splits_tool_call` when the fork point separates a tool call from its
    /// response.
    pub fn fork(&self, request: &ForkRequest) -> Result<(ImportSessionRequest, HistoryChange)> {
        let source = &request.source;
        let keep = match &request.at_event_id {
            Some(event_id) => self.position(source, event_id)? + 1,
            None => self.events.len(),
        };
        let events = self.events[..keep].to_vec();
        refuse_held_calls(source, &events, "forked")?;
        refuse_split_tool_calls(source, &self.events, &events)?;

        let session_id = match &request.session_id {
            Some(id) => adk_core::identity::SessionId::try_from(id.as_str())?,
            None => adk_core::identity::SessionId::try_from(uuid::Uuid::new_v4().to_string())?,
        };
        let identity =
            AdkIdentity::new(source.app_name.clone(), source.user_id.clone(), session_id);
        let origin = ForkOrigin {
            session_id: source.session_id.as_ref().to_string(),
            event_id: events.last().map(|event| event.id.clone()),
            forked_at: Utc::now(),
        };
        let origin_value = serde_json::to_value(&origin).map_err(|e| {
            history_error(
                ErrorCategory::Internal,
                "session.history.serialize",
                format!("serialize fork origin failed: {e}"),
            )
        })?;
        // A fork is a new, active session: it keeps neither the source's archive mark nor the
        // events that set it, so a later rewind of the fork cannot archive it again.
        let events: Vec<Event> =
            events.into_iter().filter(|event| !crate::archive::is_marker_event(event)).collect();
        let mut initial_state = self.creation_state();
        initial_state.remove(crate::archive::ARCHIVE_KEY);
        initial_state.insert(FORK_ORIGIN_KEY.to_string(), origin_value.clone());
        let mut state = self.state_after(&events);
        state.remove(crate::archive::ARCHIVE_KEY);
        state.insert(FORK_ORIGIN_KEY.to_string(), origin_value);

        let change = HistoryChange {
            operation: HistoryOperation::Fork,
            session_id: identity.session_id.as_ref().to_string(),
            version_before: self.version(),
            version_after: history_version(&events),
            removed: Vec::new(),
            replaced: Vec::new(),
            changed_digest: None,
            events_after: events.len(),
            fork_origin: Some(origin),
        };
        Ok((ImportSessionRequest { identity, initial_state, state, events }, change))
    }
}

/// What the session holds after a [`SessionHistory::rewrite`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RewrittenHistory {
    /// Every event the session holds afterwards, in order.
    pub events: Vec<Event>,
    /// The session's own state afterwards.
    pub state: StateMap,
    /// What changed.
    pub change: HistoryChange,
}

/// An in-place change to a session's history.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HistoryRewrite {
    /// Keeps the events up to and including `event_id` and removes the rest.
    RewindTo {
        /// The last event to keep.
        event_id: String,
    },
    /// Removes the last `steps` events.
    RewindSteps {
        /// How many events to remove from the end.
        steps: usize,
    },
    /// Applies `edits` in order.
    Edit {
        /// The edits.
        edits: Vec<HistoryEdit>,
    },
}

/// One edit of a session's history.
///
/// # Example
///
/// ```rust
/// use adk_session::{Event, HistoryEdit, HistoryRewrite};
///
/// let mut corrected = Event::new("inv-1");
/// corrected.id = "event-to-correct".into();
/// let rewrite = HistoryRewrite::Edit {
///     edits: vec![
///         HistoryEdit::Replace { event: Box::new(corrected) },
///         HistoryEdit::Remove { event_id: "event-to-drop".into() },
///     ],
/// };
/// assert!(matches!(rewrite, HistoryRewrite::Edit { .. }));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum HistoryEdit {
    /// Replaces the stored event with the same ID by `event`.
    ///
    /// The replacement keeps the original's timestamp, so it stays where the original stood.
    /// Its `temp:` state keys are dropped, and its `app:` and `user:` keys are kept on the
    /// event but not applied to the shared tiers.
    Replace {
        /// The replacement; its `id` names the event it replaces.
        event: Box<Event>,
    },
    /// Removes the event `event_id`.
    Remove {
        /// The event to remove.
        event_id: String,
    },
}

/// Request to rewrite a session's history in place.
///
/// # Example
///
/// ```rust
/// use adk_core::identity::AdkIdentity;
/// use adk_session::{HistoryRewrite, RewriteHistoryRequest};
///
/// let identity = AdkIdentity::new(
///     "app".try_into().unwrap(),
///     "alice".try_into().unwrap(),
///     "s1".try_into().unwrap(),
/// );
/// let request = RewriteHistoryRequest::new(identity, HistoryRewrite::RewindSteps { steps: 2 })
///     .expect_version("fnv1a128:0");
/// assert_eq!(request.expected_version.as_deref(), Some("fnv1a128:0"));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RewriteHistoryRequest {
    /// The session to rewrite.
    pub identity: AdkIdentity,
    /// The change.
    pub rewrite: HistoryRewrite,
    /// When set, the rewrite applies only while the history's
    /// [`version`](SessionHistory::version) is still this one.
    pub expected_version: Option<String>,
}

impl RewriteHistoryRequest {
    /// A rewrite of the session `identity` that applies whatever its current version.
    pub fn new(identity: AdkIdentity, rewrite: HistoryRewrite) -> Self {
        Self { identity, rewrite, expected_version: None }
    }

    /// Applies the rewrite only while the history is still at `version`.
    #[must_use]
    pub fn expect_version(mut self, version: impl Into<String>) -> Self {
        self.expected_version = Some(version.into());
        self
    }
}

/// Request to fork a session.
///
/// The fork belongs to the same app and user as the source.
///
/// # Example
///
/// ```rust
/// use adk_core::identity::AdkIdentity;
/// use adk_session::ForkRequest;
///
/// let source = AdkIdentity::new(
///     "app".try_into().unwrap(),
///     "alice".try_into().unwrap(),
///     "main".try_into().unwrap(),
/// );
/// let request = ForkRequest::new(source).at_event("event-7").with_session_id("try-b");
/// assert_eq!(request.at_event_id.as_deref(), Some("event-7"));
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ForkRequest {
    /// The session to fork.
    pub source: AdkIdentity,
    /// The last event the fork copies; `None` copies every event.
    pub at_event_id: Option<String>,
    /// The fork's session ID; a UUID when `None`.
    pub session_id: Option<String>,
}

impl ForkRequest {
    /// A fork of every event of `source` under a generated session ID.
    pub fn new(source: AdkIdentity) -> Self {
        Self { source, at_event_id: None, session_id: None }
    }

    /// Copies the events up to and including `event_id` only.
    #[must_use]
    pub fn at_event(mut self, event_id: impl Into<String>) -> Self {
        self.at_event_id = Some(event_id.into());
        self
    }

    /// Names the fork's session.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

/// Request to create a session that already holds events.
///
/// Unlike [`create`](crate::SessionService::create) followed by appends, an import writes the
/// events as they are and never applies their state deltas to the `app:` and `user:` tiers;
/// `state` and `initial_state` hold only the session's own keys.
/// [`fork`](crate::SessionService::fork) builds one; it also moves a session between backends.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImportSessionRequest {
    /// The session to create.
    pub identity: AdkIdentity,
    /// The state recorded as the one the session was created with.
    pub initial_state: StateMap,
    /// The session's state.
    pub state: StateMap,
    /// The events, in order.
    pub events: Vec<Event>,
}

impl ImportSessionRequest {
    /// An import of `events` into the new session `identity`, with `state` as both its current
    /// state and the state it was created with.
    pub fn new(identity: AdkIdentity, state: StateMap, events: Vec<Event>) -> Self {
        Self { identity, initial_state: state.clone(), state, events }
    }
}

/// Which operation changed a history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum HistoryOperation {
    /// A fork created a session.
    Fork,
    /// A rewind removed the events after a target.
    Rewind,
    /// An edit replaced or removed events.
    Edit,
}

impl HistoryOperation {
    /// The audit action name: `session.fork`, `session.rewind`, or `session.edit`.
    pub fn action(self) -> &'static str {
        match self {
            Self::Fork => "session.fork",
            Self::Rewind => "session.rewind",
            Self::Edit => "session.edit",
        }
    }
}

/// What a fork, rewind, or edit changed, for the audit log.
///
/// It names events by ID and carries a digest of the removed and replaced events, never their
/// content, so a record of a redaction does not repeat what was redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct HistoryChange {
    /// The operation.
    pub operation: HistoryOperation,
    /// The session the operation wrote: the rewritten session, or the new fork.
    pub session_id: String,
    /// The source's [`version`](SessionHistory::version) before the operation.
    pub version_before: String,
    /// The written session's version afterwards.
    pub version_after: String,
    /// The removed events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    /// The replaced events.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<String>,
    /// Digest of the removed and replaced events as they were before the operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_digest: Option<String>,
    /// How many events the written session holds afterwards.
    pub events_after: usize,
    /// For a fork, where it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_origin: Option<ForkOrigin>,
}

/// A session after a history operation, with what the operation changed.
#[non_exhaustive]
pub struct HistoryOutcome {
    /// The session as it is afterwards: the rewritten session, or the new fork.
    pub session: Box<dyn Session>,
    /// What changed.
    pub change: HistoryChange,
}

impl HistoryOutcome {
    /// Pairs a session with the change that produced it.
    pub fn new(session: Box<dyn Session>, change: HistoryChange) -> Self {
        Self { session, change }
    }
}

impl std::fmt::Debug for HistoryOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryOutcome")
            .field("session_id", &self.session.id())
            .field("change", &self.change)
            .finish()
    }
}

/// The version digest of `events`.
pub(crate) fn history_version(events: &[Event]) -> String {
    let entries: Vec<&Event> = events.iter().collect();
    events_digest(&entries)
}

fn events_digest(events: &[&Event]) -> String {
    let value = serde_json::to_value(events).unwrap_or(Value::Null);
    adk_core::json_digest(&value)
}

fn history_error(category: ErrorCategory, code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Session, category, code, message)
}

fn event_not_found(identity: &AdkIdentity, event_id: &str) -> AdkError {
    crate::service::rewind_event_not_found(identity, event_id)
}

/// Fails while `events` hold a call for a decision.
fn refuse_held_calls(identity: &AdkIdentity, events: &[Event], verb: &str) -> Result<()> {
    if adk_core::held_call_compaction_limit(events, events.len()) < events.len() {
        return Err(history_error(
            ErrorCategory::Conflict,
            "session.history.held_call",
            format!(
                "session '{}' holds a tool call waiting for a decision, so its history cannot be \
                 {verb}; approve or reject the call first, or fork at an event before it",
                identity.session_id
            ),
        ));
    }
    Ok(())
}

/// The IDs of the function calls and the function responses in `events`.
fn call_and_response_ids(events: &[Event]) -> (HashSet<&str>, HashSet<&str>) {
    let mut calls = HashSet::new();
    let mut responses = HashSet::new();
    for part in events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
    {
        match part {
            Part::FunctionCall { id: Some(id), .. } => {
                calls.insert(id.as_str());
            }
            Part::FunctionResponse { id: Some(id), .. } => {
                responses.insert(id.as_str());
            }
            _ => {}
        }
    }
    (calls, responses)
}

/// Fails when `after` keeps a tool call or response whose partner `before` held and `after`
/// lost.
fn refuse_split_tool_calls(
    identity: &AdkIdentity,
    before: &[Event],
    after: &[Event],
) -> Result<()> {
    let (calls_before, responses_before) = call_and_response_ids(before);
    let (calls_after, responses_after) = call_and_response_ids(after);
    let mut split: Vec<&str> = calls_before
        .intersection(&responses_before)
        .filter(|id| calls_after.contains(*id) != responses_after.contains(*id))
        .copied()
        .collect();
    if split.is_empty() {
        return Ok(());
    }
    split.sort_unstable();
    Err(history_error(
        ErrorCategory::InvalidInput,
        "session.history.splits_tool_call",
        format!(
            "the change to session '{}' would keep tool call {} without its response or the \
             response without its call; keep or remove both events, or choose an event before \
             the call",
            identity.session_id,
            split.join(", ")
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, FunctionResponseData};
    use serde_json::json;

    fn identity() -> AdkIdentity {
        AdkIdentity::new(
            "app".try_into().unwrap(),
            "alice".try_into().unwrap(),
            "s1".try_into().unwrap(),
        )
    }

    fn event(id: &str, delta: &[(&str, Value)]) -> Event {
        let mut event = Event::new("inv");
        event.id = id.to_string();
        event.actions.state_delta =
            delta.iter().map(|(key, value)| (key.to_string(), value.clone())).collect();
        event
    }

    fn call(id: &str, call_id: &str) -> Event {
        let mut event = event(id, &[]);
        event.llm_response.content = Some(Content {
            role: "model".into(),
            parts: vec![Part::FunctionCall {
                name: "lookup".into(),
                args: json!({}),
                id: Some(call_id.into()),
                thought_signature: None,
            }],
        });
        event
    }

    fn response(id: &str, call_id: &str) -> Event {
        let mut event = event(id, &[]);
        event.llm_response.content = Some(Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"ok": true})),
                id: Some(call_id.into()),
                annotations: None,
            }],
        });
        event
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|event| event.id.as_str()).collect()
    }

    #[test]
    fn rebuilds_state_from_the_recorded_creation_state() {
        let history = SessionHistory::new(
            vec![
                event("e1", &[("step", json!(1)), ("app:hits", json!(1))]),
                event("e2", &[("step", json!(2)), ("tier", json!("silver"))]),
            ],
            HashMap::from([("step".into(), json!(2)), ("tier".into(), json!("silver"))]),
            Some(HashMap::from([("tier".into(), json!("gold"))])),
        );
        let rewound =
            history.rewrite(&identity(), &HistoryRewrite::RewindSteps { steps: 1 }, None).unwrap();
        assert_eq!(
            rewound.state,
            HashMap::from([("step".into(), json!(1)), ("tier".into(), json!("gold"))])
        );
        assert_eq!(rewound.change.removed, ["e2"]);
    }

    #[test]
    fn a_session_without_a_recorded_creation_state_keeps_keys_no_event_wrote() {
        let history = SessionHistory::new(
            vec![event("e1", &[("step", json!(1))])],
            HashMap::from([("step".into(), json!(1)), ("owner".into(), json!("alice"))]),
            None,
        );
        assert_eq!(history.creation_state(), HashMap::from([("owner".into(), json!("alice"))]));
    }

    #[test]
    fn a_backend_without_shared_tiers_keeps_app_and_user_keys_in_the_session() {
        let history = SessionHistory::without_shared_tiers(
            vec![event("e1", &[("app:hits", json!(1)), ("temp:x", json!(1))])],
            HashMap::new(),
            Some(HashMap::new()),
        );
        assert_eq!(
            history.state_after(&history.events),
            HashMap::from([("app:hits".into(), json!(1))])
        );
    }

    #[test]
    fn edits_replace_in_place_and_remove() {
        let history = SessionHistory::new(
            vec![event("e1", &[]), event("e2", &[("draft", json!("v1"))]), event("e3", &[])],
            HashMap::new(),
            Some(HashMap::new()),
        );
        let mut replacement = event("e2", &[("draft", json!("v2")), ("temp:scratch", json!(1))]);
        replacement.timestamp = Utc::now() + chrono::Duration::days(1);
        let edited = history
            .rewrite(
                &identity(),
                &HistoryRewrite::Edit {
                    edits: vec![
                        HistoryEdit::Replace { event: Box::new(replacement) },
                        HistoryEdit::Remove { event_id: "e3".into() },
                    ],
                },
                None,
            )
            .unwrap();
        assert_eq!(ids(&edited.events), ["e1", "e2"]);
        assert_eq!(edited.events[1].timestamp, history.events[1].timestamp);
        assert_eq!(edited.state, HashMap::from([("draft".into(), json!("v2"))]));
        assert_eq!(
            (edited.change.replaced.as_slice(), edited.change.removed.as_slice()),
            (&["e2".to_string()][..], &["e3".to_string()][..])
        );
        assert!(edited.change.changed_digest.is_some());
    }

    #[test]
    fn a_stale_version_is_refused() {
        let history = SessionHistory::new(vec![event("e1", &[])], HashMap::new(), None);
        let error = history
            .rewrite(&identity(), &HistoryRewrite::RewindSteps { steps: 1 }, Some("stale"))
            .unwrap_err();
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::Conflict, "session.history.version_mismatch")
        );
        let version = history.version();
        assert!(
            history
                .rewrite(&identity(), &HistoryRewrite::RewindSteps { steps: 1 }, Some(&version))
                .is_ok()
        );
    }

    #[test]
    fn tool_calls_stay_with_their_responses() {
        let history = SessionHistory::new(
            vec![
                event("ask", &[]),
                call("call", "c1"),
                response("result", "c1"),
                event("reply", &[]),
            ],
            HashMap::new(),
            None,
        );
        for rewrite in [
            HistoryRewrite::RewindTo { event_id: "call".into() },
            HistoryRewrite::Edit { edits: vec![HistoryEdit::Remove { event_id: "call".into() }] },
            HistoryRewrite::Edit { edits: vec![HistoryEdit::Remove { event_id: "result".into() }] },
        ] {
            let error = history.rewrite(&identity(), &rewrite, None).unwrap_err();
            assert_eq!(error.code, "session.history.splits_tool_call", "{rewrite:?}");
        }
        let both = history
            .rewrite(
                &identity(),
                &HistoryRewrite::Edit {
                    edits: vec![
                        HistoryEdit::Remove { event_id: "call".into() },
                        HistoryEdit::Remove { event_id: "result".into() },
                    ],
                },
                None,
            )
            .unwrap();
        assert_eq!(ids(&both.events), ["ask", "reply"]);
        let error = history.fork(&ForkRequest::new(identity()).at_event("call")).unwrap_err();
        assert_eq!(error.code, "session.history.splits_tool_call");
    }

    #[test]
    fn a_held_call_blocks_rewrites_and_forks_that_copy_it() {
        let mut hold = call("hold", "c9");
        hold.actions.tool_confirmation = Some(adk_core::ToolConfirmationRequest {
            tool_name: "lookup".into(),
            function_call_id: Some("c9".into()),
            args: json!({}),
            acting: None,
            origin: None,
        });
        let history = SessionHistory::new(vec![event("ask", &[]), hold], HashMap::new(), None);
        let error = history
            .rewrite(&identity(), &HistoryRewrite::RewindSteps { steps: 1 }, None)
            .unwrap_err();
        assert_eq!(
            (error.category, error.code),
            (ErrorCategory::Conflict, "session.history.held_call")
        );
        assert_eq!(
            history.fork(&ForkRequest::new(identity())).unwrap_err().code,
            "session.history.held_call"
        );
        let (import, _) = history.fork(&ForkRequest::new(identity()).at_event("ask")).unwrap();
        assert_eq!(ids(&import.events), ["ask"]);
    }

    #[test]
    fn a_fork_records_its_origin_in_both_states() {
        let history = SessionHistory::new(
            vec![event("e1", &[("step", json!(1))]), event("e2", &[("step", json!(2))])],
            HashMap::from([("step".into(), json!(2))]),
            Some(HashMap::from([("owner".into(), json!("alice"))])),
        );
        let (import, change) = history
            .fork(&ForkRequest::new(identity()).at_event("e1").with_session_id("fork-1"))
            .unwrap();
        assert_eq!(import.identity.session_id.as_ref(), "fork-1");
        assert_eq!(ids(&import.events), ["e1"]);
        let origin: ForkOrigin =
            serde_json::from_value(import.state[FORK_ORIGIN_KEY].clone()).unwrap();
        assert_eq!((origin.session_id.as_str(), origin.event_id.as_deref()), ("s1", Some("e1")));
        assert_eq!(import.initial_state[FORK_ORIGIN_KEY], import.state[FORK_ORIGIN_KEY]);
        assert_eq!(
            (import.state.get("step"), import.state.get("owner")),
            (Some(&json!(1)), Some(&json!("alice")))
        );
        assert_eq!(change.fork_origin, Some(origin));
    }

    #[test]
    fn duplicated_event_ids_cannot_be_addressed() {
        let history =
            SessionHistory::new(vec![event("e1", &[]), event("e1", &[])], HashMap::new(), None);
        let error = history
            .rewrite(&identity(), &HistoryRewrite::RewindTo { event_id: "e1".into() }, None)
            .unwrap_err();
        assert_eq!(error.code, "session.history.ambiguous_event");
    }
}
