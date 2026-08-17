//! Agent Engine dispatch surface for the Gemini Enterprise Agent Platform.
//!
//! Makes an adk-rust agent in a BYOC container drivable by
//! `reasoningEngines.query`, `reasoningEngines.streamQuery`, the console
//! Playground, and the platform SDKs. The host POSTs a
//! [`DispatchRequest`] envelope naming a [`ClassMethod`]; this module routes
//! it to the session service, memory service, or [`Runner`].
//!
//! - `POST /api/reasoning_engine` — unary operations, responding
//!   `{"output": ...}`.
//! - `POST /api/stream_reasoning_engine` — streaming operations, responding
//!   newline-delimited JSON events with `Content-Type: application/json`
//!   (no SSE framing).
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_server::agent_engine::{AgentEngineState, agent_engine_router};
//! use adk_runner::Runner;
//! use std::sync::Arc;
//!
//! # fn build(runner: Arc<Runner>) -> axum::Router {
//! let state = AgentEngineState::new(runner);
//! agent_engine_router(state)
//! # }
//! ```

mod entrypoint;
mod envelope;
mod operations;

pub use entrypoint::{AgentEngineOptions, build_agent_engine_app, serve_agent_engine};
pub use envelope::DispatchRequest;
pub use operations::{
    AddSessionToMemoryInput, AgentRunRequest, ApiMode, ClassMethod, CreateSessionInput,
    DeleteSessionInput, GetSessionInput, ListSessionsInput, SearchMemoryInput, StreamQueryInput,
    StreamingAgentRunWithEventsInput,
};

use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Result};
use adk_runner::Runner;
use axum::{
    Json, Router,
    body::Body,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{error, info};

/// Shared state for the Agent Engine dispatch handlers.
///
/// The session service and app name are taken from the [`Runner`] so the
/// dispatch surface and the runner can never disagree about session scoping.
/// The memory and artifact services are optional from day one: filling them
/// in later is additive, mirroring adk-python `AdkApp`'s
/// `memory_service_builder` / `artifact_service_builder`.
#[derive(Clone)]
pub struct AgentEngineState {
    runner: Arc<Runner>,
    session_service: Arc<dyn adk_session::SessionService>,
    memory_service: Option<Arc<dyn adk_memory::MemoryService>>,
    artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    app_name: String,
}

impl AgentEngineState {
    /// Creates dispatch state around a prebuilt runner.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_server::agent_engine::AgentEngineState;
    /// use adk_runner::Runner;
    /// use std::sync::Arc;
    ///
    /// # fn build(runner: Arc<Runner>) {
    /// let state = AgentEngineState::new(runner);
    /// # }
    /// ```
    pub fn new(runner: Arc<Runner>) -> Self {
        let session_service = runner.session_service().clone();
        let app_name = runner.app_name().to_string();
        Self { runner, session_service, memory_service: None, artifact_service: None, app_name }
    }

    /// Configures the memory service backing `async_add_session_to_memory`
    /// and `async_search_memory`. Without one, both return an
    /// [`Unsupported`](adk_core::ErrorCategory::Unsupported) error.
    pub fn with_memory_service(
        mut self,
        memory_service: Arc<dyn adk_memory::MemoryService>,
    ) -> Self {
        self.memory_service = Some(memory_service);
        self
    }

    /// Configures the artifact service. No class method consumes it yet; it
    /// is carried here so wiring `GcsArtifactService` later is additive.
    pub fn with_artifact_service(
        mut self,
        artifact_service: Arc<dyn adk_artifact::ArtifactService>,
    ) -> Self {
        self.artifact_service = Some(artifact_service);
        self
    }

    /// The runner executing `stream_query` operations.
    pub fn runner(&self) -> &Arc<Runner> {
        &self.runner
    }

    /// The session service backing the session class methods.
    pub fn session_service(&self) -> &Arc<dyn adk_session::SessionService> {
        &self.session_service
    }

    /// The memory service, when configured.
    pub fn memory_service(&self) -> Option<&Arc<dyn adk_memory::MemoryService>> {
        self.memory_service.as_ref()
    }

    /// The artifact service, when configured.
    pub fn artifact_service(&self) -> Option<&Arc<dyn adk_artifact::ArtifactService>> {
        self.artifact_service.as_ref()
    }

    /// The resolved application name used for session scoping.
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

/// Builds the Agent Engine dispatch router.
///
/// Mounts `POST /api/reasoning_engine` (unary) and
/// `POST /api/stream_reasoning_engine` (streaming). Merge it into a server
/// app or serve it standalone in a BYOC container.
pub fn agent_engine_router(state: AgentEngineState) -> Router {
    Router::new()
        .route("/api/reasoning_engine", post(dispatch_unary))
        .route("/api/stream_reasoning_engine", post(dispatch_stream))
        .with_state(state)
}

/// Renders an [`AdkError`] as its problem-JSON response.
fn problem_response(err: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(err.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(err.to_problem_json())).into_response()
}

/// The error for a method POSTed to the wrong endpoint (streaming method on
/// the unary route or vice versa).
fn wrong_endpoint(method: ClassMethod, expected: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        "agent_engine.wrong_endpoint",
        format!("class_method '{}' must be POSTed to {expected}", method.as_str()),
    )
}

/// Unary dispatch: `POST /api/reasoning_engine`.
async fn dispatch_unary(
    axum::extract::State(state): axum::extract::State<AgentEngineState>,
    Json(request): Json<DispatchRequest>,
) -> Response {
    info!(class_method = %request.class_method, "agent engine unary dispatch");
    let method = match ClassMethod::from_str(&request.class_method) {
        Ok(method) => method,
        Err(err) => return problem_response(&err),
    };
    if method.api_mode().is_streaming() {
        return problem_response(&wrong_endpoint(method, "/api/stream_reasoning_engine"));
    }
    let output = run_unary(&state, method, request.input).await;
    match output {
        Ok(output) => Json(envelope::unary_response(output)).into_response(),
        Err(err) => {
            error!(class_method = %request.class_method, error = %err, "unary dispatch failed");
            problem_response(&err)
        }
    }
}

/// Executes a unary class method against the state.
///
/// The match is exhaustive — streaming variants are rejected before this is
/// called, and returning an error for them here keeps the compiler enforcing
/// that every future variant picks an endpoint.
async fn run_unary(
    state: &AgentEngineState,
    method: ClassMethod,
    input: Option<Value>,
) -> Result<Value> {
    match method {
        ClassMethod::CreateSession | ClassMethod::AsyncCreateSession => {
            operations::handle_create_session(state, operations::typed_input(input)?).await
        }
        ClassMethod::GetSession | ClassMethod::AsyncGetSession => {
            operations::handle_get_session(state, operations::typed_input(input)?).await
        }
        ClassMethod::ListSessions | ClassMethod::AsyncListSessions => {
            operations::handle_list_sessions(state, operations::typed_input(input)?).await
        }
        ClassMethod::DeleteSession | ClassMethod::AsyncDeleteSession => {
            operations::handle_delete_session(state, operations::typed_input(input)?).await
        }
        ClassMethod::AsyncAddSessionToMemory => {
            operations::handle_add_session_to_memory(state, operations::typed_input(input)?).await
        }
        ClassMethod::AsyncSearchMemory => {
            operations::handle_search_memory(state, operations::typed_input(input)?).await
        }
        ClassMethod::RegisterOperations => Ok(operations::handle_register_operations()),
        ClassMethod::StreamQuery
        | ClassMethod::AsyncStreamQuery
        | ClassMethod::StreamingAgentRunWithEvents => {
            Err(wrong_endpoint(method, "/api/stream_reasoning_engine"))
        }
    }
}

/// Streaming dispatch: `POST /api/stream_reasoning_engine`.
async fn dispatch_stream(
    axum::extract::State(state): axum::extract::State<AgentEngineState>,
    Json(request): Json<DispatchRequest>,
) -> Response {
    info!(class_method = %request.class_method, "agent engine streaming dispatch");
    let method = match ClassMethod::from_str(&request.class_method) {
        Ok(method) => method,
        Err(err) => return problem_response(&err),
    };
    let prepared = match method {
        ClassMethod::StreamQuery | ClassMethod::AsyncStreamQuery => {
            prepare_stream_query(&state, request.input).await
        }
        ClassMethod::StreamingAgentRunWithEvents => {
            prepare_agent_run_with_events(&state, request.input).await
        }
        ClassMethod::CreateSession
        | ClassMethod::AsyncCreateSession
        | ClassMethod::GetSession
        | ClassMethod::AsyncGetSession
        | ClassMethod::ListSessions
        | ClassMethod::AsyncListSessions
        | ClassMethod::DeleteSession
        | ClassMethod::AsyncDeleteSession
        | ClassMethod::AsyncAddSessionToMemory
        | ClassMethod::AsyncSearchMemory
        | ClassMethod::RegisterOperations => Err(wrong_endpoint(method, "/api/reasoning_engine")),
    };
    let (user_id, session_id, content) = match prepared {
        Ok(prepared) => prepared,
        Err(err) => {
            error!(class_method = %request.class_method, error = %err, "streaming dispatch failed");
            return problem_response(&err);
        }
    };

    let (typed_user_id, typed_session_id) = match operations::typed_identity(&user_id, &session_id)
    {
        Ok(identity) => identity,
        Err(err) => return problem_response(&err),
    };
    let event_stream = match state.runner().run(typed_user_id, typed_session_id, content).await {
        Ok(stream) => stream,
        Err(err) => {
            error!(error = %err, "runner failed to start");
            return problem_response(&err);
        }
    };

    // One JSON object per line. An error after the first event cannot change
    // the already-sent 200 status, so it is emitted as a problem-JSON line
    // and the stream ends — the same contract as adk-python's template
    // server, whose callers parse each line independently.
    let body_stream = event_stream.map(|item| {
        let line = match item {
            Ok(event) => serde_json::to_string(&event).unwrap_or_else(|err| {
                error_line(&AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Internal,
                    "agent_engine.event_serialization",
                    format!("failed to serialize event: {err}"),
                ))
            }),
            Err(err) => {
                error!(error = %err, "agent event stream failed");
                error_line(&err)
            }
        };
        Ok::<_, std::convert::Infallible>(format!("{line}\n"))
    });

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|err| {
            error!(error = %err, "failed to build streaming response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
}

/// Serializes an error as a single NDJSON problem line.
fn error_line(err: &AdkError) -> String {
    err.to_problem_json().to_string()
}

/// Validates a `stream_query` input and resolves its session.
async fn prepare_stream_query(
    state: &AgentEngineState,
    input: Option<Value>,
) -> Result<(String, String, Content)> {
    let input: StreamQueryInput = operations::typed_input(input)?;
    let content = operations::message_to_content(input.message)?;
    let (session_id, _created) =
        operations::resolve_session(state, &input.user_id, input.session_id, HashMap::new())
            .await?;
    Ok((input.user_id, session_id, content))
}

/// Validates a `streaming_agent_run_with_events` input and resolves its
/// session, applying the request's state delta.
async fn prepare_agent_run_with_events(
    state: &AgentEngineState,
    input: Option<Value>,
) -> Result<(String, String, Content)> {
    let input: StreamingAgentRunWithEventsInput = operations::typed_input(input)?;
    let request: AgentRunRequest = serde_json::from_str(&input.request_json).map_err(|err| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "agent_engine.invalid_agent_run_request",
            format!("request_json is not a valid AgentRunRequest: {err}"),
        )
    })?;
    let state_delta: HashMap<String, Value> =
        request.state_delta.unwrap_or_default().into_iter().collect();
    // A created session takes the delta as its initial state; an existing one
    // gets it as an appended state-delta event, matching the REST runtime.
    let (session_id, created) = operations::resolve_session(
        state,
        &request.user_id,
        Some(request.session_id),
        state_delta.clone(),
    )
    .await?;
    if !created {
        operations::apply_state_delta(state, &request.user_id, &session_id, state_delta).await?;
    }
    Ok((request.user_id, session_id, request.new_message))
}
