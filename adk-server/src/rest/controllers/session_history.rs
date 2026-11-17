//! Session history over REST: read, fork, rewind, and edit.
//!
//! | Route | Does |
//! |-------|------|
//! | `GET /apps/{app}/users/{user}/sessions/{session}/history` | The session with its history `version` and fork origin |
//! | `POST …/sessions/{session}/fork` | Forks the session; `201` with the fork and the change |
//! | `POST …/sessions/{session}/rewind` | Rewinds to an event, by steps, or to before an invocation |
//! | `POST …/sessions/{session}/history/edit` | Replaces or removes events |
//!
//! The routes sit behind the session routes' authentication: a caller reaches only its own
//! user's sessions. They go through [`HistoryControl`], so they refuse while a turn runs on the
//! session (`409 session.history.run_active`) and record each change in the governance audit
//! trail. Errors are problem JSON with the error code.

use std::sync::Arc;

use adk_core::audit_log::AuditActor;
use adk_core::identity::AdkIdentity;
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Event};
use adk_runner::{HistoryControl, RewindTarget, SessionActivity};
use adk_session::{ForkOrigin, ForkRequest, HistoryChange, HistoryEdit, SessionHistory};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use super::session::{SessionController, SessionResponse};

/// State of the session history routes.
#[derive(Clone)]
pub struct HistoryController {
    sessions: Arc<dyn adk_session::SessionService>,
    activity: SessionActivity,
    governance: adk_core::GovernanceControl,
}

impl HistoryController {
    /// History routes over the server's sessions, runs, and audit trail.
    pub fn new(config: &crate::ServerConfig) -> Self {
        Self {
            sessions: config.session_service.clone(),
            activity: config.session_activity.clone(),
            governance: config.governance.clone(),
        }
    }

    /// The governed history operations, with the audit trail the kill switch holds now.
    fn control(&self) -> HistoryControl {
        let control =
            HistoryControl::new(self.sessions.clone()).with_activity(self.activity.clone());
        match self.governance.audit() {
            Some(trail) => control.with_audit(trail),
            None => control,
        }
    }
}

/// An error as an HTTP status and problem JSON.
type Problem = (StatusCode, Json<serde_json::Value>);

fn problem(error: &AdkError) -> Problem {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status.is_server_error() {
        tracing::error!(error = %error, "session history request failed");
    } else {
        tracing::debug!(error = %error, "session history request refused");
    }
    (status, Json(error.to_problem_json()))
}

fn invalid(code: &'static str, message: impl Into<String>) -> Problem {
    problem(&AdkError::new(ErrorComponent::Server, ErrorCategory::InvalidInput, code, message))
}

/// The session the path names, for a caller allowed to reach it, and the actor to record.
fn target(
    request_context: &Option<adk_core::RequestContext>,
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> Result<(AdkIdentity, AuditActor), Problem> {
    if let Some(context) = request_context
        && context.user_id != user_id
    {
        return Err(problem(&AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Forbidden,
            "session.history.forbidden",
            "a caller can reach only its own user's sessions",
        )));
    }
    let identity = AdkIdentity::new(
        app_name.try_into().map_err(|e: adk_core::IdentityError| problem(&e.into()))?,
        user_id.try_into().map_err(|e: adk_core::IdentityError| problem(&e.into()))?,
        session_id.try_into().map_err(|e: adk_core::IdentityError| problem(&e.into()))?,
    );
    Ok((identity, AuditActor::subject(SubjectRef::human(user_id))))
}

/// `GET …/history`: the session, its history version, and where it was forked from.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryResponse {
    /// The session as `GET …/sessions/{session}` returns it.
    pub session: SessionResponse,
    /// The history version; pass it as `expectedVersion` to change only this history.
    pub version: String,
    /// Where the session was forked from, for a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkOrigin>,
}

/// The session after a fork, rewind, or edit, and what changed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOutcomeResponse {
    /// The rewritten session, or the new fork.
    pub session: SessionResponse,
    /// What changed: removed and replaced event IDs, versions, and the fork origin.
    pub change: HistoryChange,
}

/// Body of `POST …/fork`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkSessionBody {
    /// The last event the fork copies; every event when absent.
    #[serde(default)]
    pub event_id: Option<String>,
    /// The fork's session ID; generated when absent.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Body of `POST …/rewind`: exactly one of `eventId`, `steps`, and `beforeInvocationId`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindSessionBody {
    /// Keep the events up to and including this one.
    #[serde(default)]
    pub event_id: Option<String>,
    /// Remove this many events from the end.
    #[serde(default)]
    pub steps: Option<usize>,
    /// Remove this invocation and everything after it.
    #[serde(default)]
    pub before_invocation_id: Option<String>,
    /// Rewind only while the history is still at this version.
    #[serde(default)]
    pub expected_version: Option<String>,
}

/// One edit in `POST …/history/edit`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
pub enum HistoryEditBody {
    /// Replace the stored event with the same `id`.
    Replace {
        /// The replacement event.
        event: Box<Event>,
    },
    /// Remove the event.
    Remove {
        /// The event to remove.
        #[serde(rename = "eventId")]
        event_id: String,
    },
}

/// Body of `POST …/history/edit`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditHistoryBody {
    /// The edits, applied in order.
    pub edits: Vec<HistoryEditBody>,
    /// Edit only while the history is still at this version.
    #[serde(default)]
    pub expected_version: Option<String>,
}

fn outcome_response(outcome: adk_session::HistoryOutcome) -> HistoryOutcomeResponse {
    HistoryOutcomeResponse {
        session: SessionController::session_to_response(outcome.session.as_ref()),
        change: outcome.change,
    }
}

/// `GET /apps/{app}/users/{user}/sessions/{session}/history`.
pub async fn get_history(
    State(controller): State<HistoryController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<Json<SessionHistoryResponse>, Problem> {
    let (identity, _) = target(&request_context, &app_name, &user_id, &session_id)?;
    let session = controller.sessions.get_for_identity(&identity).await.map_err(|e| problem(&e))?;
    let version = SessionHistory::new(session.events().all(), Default::default(), None).version();
    Ok(Json(SessionHistoryResponse {
        forked_from: ForkOrigin::of(session.as_ref()),
        session: SessionController::session_to_response(session.as_ref()),
        version,
    }))
}

/// `POST /apps/{app}/users/{user}/sessions/{session}/fork`.
pub async fn fork_session(
    State(controller): State<HistoryController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    body: Option<Json<ForkSessionBody>>,
) -> Result<(StatusCode, Json<HistoryOutcomeResponse>), Problem> {
    let (identity, actor) = target(&request_context, &app_name, &user_id, &session_id)?;
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let mut request = ForkRequest::new(identity);
    if let Some(event_id) = body.event_id {
        request = request.at_event(event_id);
    }
    if let Some(fork_id) = body.session_id {
        request = request.with_session_id(fork_id);
    }
    let outcome = controller.control().fork(&actor, request).await.map_err(|e| problem(&e))?;
    tracing::info!(
        session.id = %session_id,
        fork.id = %outcome.session.id(),
        "session forked"
    );
    Ok((StatusCode::CREATED, Json(outcome_response(outcome))))
}

/// `POST /apps/{app}/users/{user}/sessions/{session}/rewind`.
pub async fn rewind_session(
    State(controller): State<HistoryController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    Json(body): Json<RewindSessionBody>,
) -> Result<Json<HistoryOutcomeResponse>, Problem> {
    let (identity, actor) = target(&request_context, &app_name, &user_id, &session_id)?;
    let rewind_target = match (body.event_id, body.steps, body.before_invocation_id) {
        (Some(event_id), None, None) => RewindTarget::Event(event_id),
        (None, Some(steps), None) => RewindTarget::Steps(steps),
        (None, None, Some(invocation_id)) => RewindTarget::BeforeInvocation(invocation_id),
        _ => {
            return Err(invalid(
                "session.rewind.target_required",
                "name exactly one of eventId, steps, and beforeInvocationId",
            ));
        }
    };
    let outcome = controller
        .control()
        .rewind(&actor, &identity, rewind_target, body.expected_version)
        .await
        .map_err(|e| problem(&e))?;
    tracing::info!(session.id = %session_id, "session rewound");
    Ok(Json(outcome_response(outcome)))
}

/// `POST /apps/{app}/users/{user}/sessions/{session}/history/edit`.
pub async fn edit_history(
    State(controller): State<HistoryController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
    Json(body): Json<EditHistoryBody>,
) -> Result<Json<HistoryOutcomeResponse>, Problem> {
    let (identity, actor) = target(&request_context, &app_name, &user_id, &session_id)?;
    if body.edits.is_empty() {
        return Err(invalid("session.history.no_edits", "name at least one edit"));
    }
    let edits = body
        .edits
        .into_iter()
        .map(|edit| match edit {
            HistoryEditBody::Replace { event } => HistoryEdit::Replace { event },
            HistoryEditBody::Remove { event_id } => HistoryEdit::Remove { event_id },
        })
        .collect();
    let outcome = controller
        .control()
        .edit(&actor, &identity, edits, body.expected_version)
        .await
        .map_err(|e| problem(&e))?;
    tracing::info!(session.id = %session_id, "session history edited");
    Ok(Json(outcome_response(outcome)))
}
