//! Class-method dispatch for the Agent Engine runtime contract.
//!
//! # Anti-corruption layer
//!
//! This module deliberately contains a Python-reflection-shaped protocol: the
//! platform dispatches on method-name strings because adk-python resolves
//! operations with `getattr`. The foreignness stays here — typed request
//! structs are produced at the boundary and nothing string-dispatched leaks
//! into `adk-core` or `adk-runner`.
//!
//! # Sync/async name pairs
//!
//! `create_session` / `async_create_session` (and the other session CRUD
//! pairs) are wire-parity aliases mapping to the same handler. adk-rust is
//! async throughout; the [`ApiMode`] split exists only because adk-python
//! exposes both sync and async Python methods and the platform routes them
//! through different API modes.
//!
//! # asyncQuery (durable query jobs)
//!
//! The platform also exposes `reasoningEngines:asyncQuery`
//! (`AsyncQueryReasoningEngine`). It is **not** registered here: the
//! capability must be declared at engine create time and cannot be added to
//! an engine post-create (google/adk-python#6220), and adk-python's `AdkApp`
//! does not register it either — strict parity excludes it. Deployments that
//! need durable query jobs must front the engine with their own job store.

use super::AgentEngineState;
use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Result, SessionId, UserId};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::str::FromStr;

/// Maximum number of events serialized into a session dump, mirroring the
/// cap used by the REST session controller.
const MAX_EVENTS: usize = 10_000;

/// The platform API mode an operation is registered under.
///
/// Wire values are the keys of the `register_operations` map: `""`, `async`,
/// `stream`, and `async_stream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiMode {
    /// Default (unary) mode, registered under `""`.
    Sync,
    /// Async unary mode, registered under `"async"`.
    Async,
    /// Streaming mode, registered under `"stream"`.
    Stream,
    /// Async streaming mode, registered under `"async_stream"`.
    AsyncStream,
}

impl ApiMode {
    /// Returns the wire key for this mode in the `register_operations` map.
    pub fn as_str(&self) -> &'static str {
        match self {
            ApiMode::Sync => "",
            ApiMode::Async => "async",
            ApiMode::Stream => "stream",
            ApiMode::AsyncStream => "async_stream",
        }
    }

    /// Whether operations in this mode stream newline-delimited JSON.
    pub fn is_streaming(&self) -> bool {
        match self {
            ApiMode::Sync | ApiMode::Async => false,
            ApiMode::Stream | ApiMode::AsyncStream => true,
        }
    }
}

/// One dispatchable operation of the Agent Engine contract.
///
/// The variant set is the exact operation set adk-python's `AdkApp` registers
/// (verification task V2, `google-cloud-aiplatform` 1.112 / `google-adk`
/// 2.6.3), plus `register_operations` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassMethod {
    /// `create_session` — session CRUD, default mode.
    CreateSession,
    /// `async_create_session` — wire-parity alias of [`Self::CreateSession`].
    AsyncCreateSession,
    /// `get_session` — session CRUD, default mode.
    GetSession,
    /// `async_get_session` — wire-parity alias of [`Self::GetSession`].
    AsyncGetSession,
    /// `list_sessions` — session CRUD, default mode.
    ListSessions,
    /// `async_list_sessions` — wire-parity alias of [`Self::ListSessions`].
    AsyncListSessions,
    /// `delete_session` — session CRUD, default mode.
    DeleteSession,
    /// `async_delete_session` — wire-parity alias of [`Self::DeleteSession`].
    AsyncDeleteSession,
    /// `stream_query` — run the agent, streaming ADK events.
    StreamQuery,
    /// `async_stream_query` — wire-parity alias of [`Self::StreamQuery`].
    AsyncStreamQuery,
    /// `streaming_agent_run_with_events` — run the agent from an ADK
    /// `AgentRunRequest` JSON string (console Playground path).
    StreamingAgentRunWithEvents,
    /// `async_add_session_to_memory` — extract a session's events into the
    /// configured memory service.
    AsyncAddSessionToMemory,
    /// `async_search_memory` — search the configured memory service.
    AsyncSearchMemory,
    /// `register_operations` — advertise the operation map to the host.
    RegisterOperations,
}

impl ClassMethod {
    /// Every dispatchable operation, in registration order.
    ///
    /// The `register_operations` output is derived from this list, so the
    /// advertised operation set and the dispatcher cannot drift.
    pub const ALL: [ClassMethod; 14] = [
        ClassMethod::CreateSession,
        ClassMethod::AsyncCreateSession,
        ClassMethod::GetSession,
        ClassMethod::AsyncGetSession,
        ClassMethod::ListSessions,
        ClassMethod::AsyncListSessions,
        ClassMethod::DeleteSession,
        ClassMethod::AsyncDeleteSession,
        ClassMethod::StreamQuery,
        ClassMethod::AsyncStreamQuery,
        ClassMethod::StreamingAgentRunWithEvents,
        ClassMethod::AsyncAddSessionToMemory,
        ClassMethod::AsyncSearchMemory,
        ClassMethod::RegisterOperations,
    ];

    /// Returns the wire name of this operation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassMethod::CreateSession => "create_session",
            ClassMethod::AsyncCreateSession => "async_create_session",
            ClassMethod::GetSession => "get_session",
            ClassMethod::AsyncGetSession => "async_get_session",
            ClassMethod::ListSessions => "list_sessions",
            ClassMethod::AsyncListSessions => "async_list_sessions",
            ClassMethod::DeleteSession => "delete_session",
            ClassMethod::AsyncDeleteSession => "async_delete_session",
            ClassMethod::StreamQuery => "stream_query",
            ClassMethod::AsyncStreamQuery => "async_stream_query",
            ClassMethod::StreamingAgentRunWithEvents => "streaming_agent_run_with_events",
            ClassMethod::AsyncAddSessionToMemory => "async_add_session_to_memory",
            ClassMethod::AsyncSearchMemory => "async_search_memory",
            ClassMethod::RegisterOperations => "register_operations",
        }
    }

    /// Returns the platform API mode this operation is registered under.
    pub fn api_mode(&self) -> ApiMode {
        match self {
            ClassMethod::CreateSession
            | ClassMethod::GetSession
            | ClassMethod::ListSessions
            | ClassMethod::DeleteSession
            | ClassMethod::RegisterOperations => ApiMode::Sync,
            ClassMethod::AsyncCreateSession
            | ClassMethod::AsyncGetSession
            | ClassMethod::AsyncListSessions
            | ClassMethod::AsyncDeleteSession
            | ClassMethod::AsyncAddSessionToMemory
            | ClassMethod::AsyncSearchMemory => ApiMode::Async,
            ClassMethod::StreamQuery => ApiMode::Stream,
            ClassMethod::AsyncStreamQuery | ClassMethod::StreamingAgentRunWithEvents => {
                ApiMode::AsyncStream
            }
        }
    }
}

impl FromStr for ClassMethod {
    type Err = AdkError;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "create_session" => Ok(ClassMethod::CreateSession),
            "async_create_session" => Ok(ClassMethod::AsyncCreateSession),
            "get_session" => Ok(ClassMethod::GetSession),
            "async_get_session" => Ok(ClassMethod::AsyncGetSession),
            "list_sessions" => Ok(ClassMethod::ListSessions),
            "async_list_sessions" => Ok(ClassMethod::AsyncListSessions),
            "delete_session" => Ok(ClassMethod::DeleteSession),
            "async_delete_session" => Ok(ClassMethod::AsyncDeleteSession),
            "stream_query" => Ok(ClassMethod::StreamQuery),
            "async_stream_query" => Ok(ClassMethod::AsyncStreamQuery),
            "streaming_agent_run_with_events" => Ok(ClassMethod::StreamingAgentRunWithEvents),
            "async_add_session_to_memory" => Ok(ClassMethod::AsyncAddSessionToMemory),
            "async_search_memory" => Ok(ClassMethod::AsyncSearchMemory),
            "register_operations" => Ok(ClassMethod::RegisterOperations),
            unknown => Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "agent_engine.unknown_class_method",
                format!(
                    "unknown class_method '{unknown}'; call register_operations for the \
                     supported operation set"
                ),
            )),
        }
    }
}

// ── Typed inputs ─────────────────────────────────────────────────────────
//
// Each operation deserializes its `input` value into one of these structs
// immediately at the dispatch boundary; handlers never see raw JSON.

/// Input for `create_session` / `async_create_session`.
#[derive(Debug, Deserialize)]
pub struct CreateSessionInput {
    /// Session owner.
    pub user_id: String,
    /// Optional caller-chosen session ID; generated when absent.
    pub session_id: Option<String>,
    /// Optional initial session state.
    pub state: Option<Map<String, Value>>,
}

/// Input for `get_session` / `async_get_session`.
#[derive(Debug, Deserialize)]
pub struct GetSessionInput {
    /// Session owner.
    pub user_id: String,
    /// Session to retrieve.
    pub session_id: String,
}

/// Input for `list_sessions` / `async_list_sessions`.
#[derive(Debug, Deserialize)]
pub struct ListSessionsInput {
    /// User whose sessions are listed.
    pub user_id: String,
}

/// Input for `delete_session` / `async_delete_session`.
#[derive(Debug, Deserialize)]
pub struct DeleteSessionInput {
    /// Session owner.
    pub user_id: String,
    /// Session to delete.
    pub session_id: String,
}

/// Input for `stream_query` / `async_stream_query`.
#[derive(Debug, Deserialize)]
pub struct StreamQueryInput {
    /// Session owner.
    pub user_id: String,
    /// Optional session to continue; a new session is created when absent.
    pub session_id: Option<String>,
    /// The user message: either a plain string or a `Content` object,
    /// matching adk-python's `str | dict` parameter.
    pub message: Value,
}

/// Input for `streaming_agent_run_with_events`.
#[derive(Debug, Deserialize)]
pub struct StreamingAgentRunWithEventsInput {
    /// An ADK `AgentRunRequest` as a JSON **string** — the console Playground
    /// sends the request pre-serialized, matching adk-python's
    /// `streaming_agent_run_with_events(request_json: str)`.
    pub request_json: String,
}

/// The ADK `AgentRunRequest` carried inside
/// [`StreamingAgentRunWithEventsInput::request_json`].
///
/// google-adk's model accepts both snake_case field names and camelCase
/// aliases (`populate_by_name=True`); the serde aliases mirror that.
#[derive(Debug, Deserialize)]
pub struct AgentRunRequest {
    /// Application name; accepted for wire parity. The runner's own app name
    /// governs session scoping, so a mismatched value is not an error.
    #[serde(default, alias = "appName")]
    pub app_name: Option<String>,
    /// Session owner.
    #[serde(alias = "userId")]
    pub user_id: String,
    /// Session to run in; created when it does not exist.
    #[serde(alias = "sessionId")]
    pub session_id: String,
    /// The user message.
    #[serde(alias = "newMessage")]
    pub new_message: Content,
    /// Accepted for wire parity; HTTP framing (unary vs streaming endpoint)
    /// decides the response shape, not this flag.
    #[serde(default)]
    pub streaming: bool,
    /// Optional state delta applied to the session before the run.
    #[serde(default, alias = "stateDelta")]
    pub state_delta: Option<Map<String, Value>>,
}

/// Input for `async_add_session_to_memory`.
#[derive(Debug, Deserialize)]
pub struct AddSessionToMemoryInput {
    /// Session owner.
    pub user_id: String,
    /// Session whose events are extracted into memory.
    pub session_id: String,
}

/// Input for `async_search_memory`.
#[derive(Debug, Deserialize)]
pub struct SearchMemoryInput {
    /// User whose memory is searched.
    pub user_id: String,
    /// Search query.
    pub query: String,
}

/// Deserializes an operation's `input` into its typed request struct.
///
/// A missing `input` is treated as `{}` so operations whose fields are all
/// optional (and `register_operations`, which takes none) accept an absent
/// field.
pub(crate) fn typed_input<T: serde::de::DeserializeOwned>(input: Option<Value>) -> Result<T> {
    let value = input.unwrap_or_else(|| Value::Object(Map::new()));
    serde_json::from_value(value).map_err(|err| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "agent_engine.invalid_input",
            format!("input does not match the class_method's schema: {err}"),
        )
    })
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// Serializes a session in the `AdkApp` dump shape: snake_case keys, events
/// inline, `last_update_time` as float seconds (Python parity).
pub(crate) fn session_to_value(session: &dyn adk_session::Session) -> Value {
    let events: Vec<Value> = session
        .events()
        .all()
        .into_iter()
        .take(MAX_EVENTS)
        .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
        .collect();
    json!({
        "id": session.id(),
        "app_name": session.app_name(),
        "user_id": session.user_id(),
        "state": session.state().all(),
        "events": events,
        "last_update_time": session.last_update_time().timestamp_millis() as f64 / 1000.0,
    })
}

/// Handles `create_session` / `async_create_session`.
pub(crate) async fn handle_create_session(
    state: &AgentEngineState,
    input: CreateSessionInput,
) -> Result<Value> {
    let initial_state: HashMap<String, Value> =
        input.state.unwrap_or_default().into_iter().collect();
    let session = state
        .session_service()
        .create(adk_session::CreateRequest {
            app_name: state.app_name().to_string(),
            user_id: input.user_id,
            session_id: input.session_id,
            state: initial_state,
        })
        .await?;
    Ok(session_to_value(session.as_ref()))
}

/// Handles `get_session` / `async_get_session`.
pub(crate) async fn handle_get_session(
    state: &AgentEngineState,
    input: GetSessionInput,
) -> Result<Value> {
    let session = state
        .session_service()
        .get(adk_session::GetRequest {
            app_name: state.app_name().to_string(),
            user_id: input.user_id,
            session_id: input.session_id,
            num_recent_events: None,
            after: None,
        })
        .await?;
    Ok(session_to_value(session.as_ref()))
}

/// Handles `list_sessions` / `async_list_sessions`.
pub(crate) async fn handle_list_sessions(
    state: &AgentEngineState,
    input: ListSessionsInput,
) -> Result<Value> {
    let sessions = state
        .session_service()
        .list(adk_session::ListRequest {
            app_name: state.app_name().to_string(),
            user_id: input.user_id,
            limit: None,
            offset: None,
        })
        .await?;
    let sessions: Vec<Value> =
        sessions.iter().map(|session| session_to_value(session.as_ref())).collect();
    Ok(json!({ "sessions": sessions }))
}

/// Handles `delete_session` / `async_delete_session`.
pub(crate) async fn handle_delete_session(
    state: &AgentEngineState,
    input: DeleteSessionInput,
) -> Result<Value> {
    state
        .session_service()
        .delete(adk_session::DeleteRequest {
            app_name: state.app_name().to_string(),
            user_id: input.user_id,
            session_id: input.session_id,
        })
        .await?;
    Ok(Value::Null)
}

/// Handles `register_operations`.
///
/// The map is derived from [`ClassMethod::ALL`] grouped by
/// [`ClassMethod::api_mode`], so it cannot drift from the dispatcher.
pub(crate) fn handle_register_operations() -> Value {
    let mut map = Map::new();
    for mode in [ApiMode::Sync, ApiMode::Async, ApiMode::Stream, ApiMode::AsyncStream] {
        let names: Vec<Value> = ClassMethod::ALL
            .iter()
            .filter(|method| method.api_mode() == mode)
            .map(|method| Value::String(method.as_str().to_string()))
            .collect();
        map.insert(mode.as_str().to_string(), Value::Array(names));
    }
    Value::Object(map)
}

/// The error returned by the memory class methods when no memory service is
/// configured — every deployment until the Memory Bank backend lands.
fn memory_unavailable() -> AdkError {
    AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unsupported,
        "agent_engine.memory_unavailable",
        "no memory service is configured on this engine; configure one via \
         AgentEngineState::with_memory_service (Memory Bank support arrives with the \
         vertex memory backend)",
    )
}

/// Handles `async_add_session_to_memory`.
///
/// Fetches the session, converts its content-bearing events into
/// [`adk_memory::MemoryEntry`] items, and adds them to the memory service —
/// the trait takes pre-extracted entries, not a session.
pub(crate) async fn handle_add_session_to_memory(
    state: &AgentEngineState,
    input: AddSessionToMemoryInput,
) -> Result<Value> {
    let Some(memory_service) = state.memory_service() else {
        return Err(memory_unavailable());
    };
    let session = state
        .session_service()
        .get(adk_session::GetRequest {
            app_name: state.app_name().to_string(),
            user_id: input.user_id.clone(),
            session_id: input.session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let entries: Vec<adk_memory::MemoryEntry> = session
        .events()
        .all()
        .into_iter()
        .filter_map(|event| {
            event.llm_response.content.clone().map(|content| adk_memory::MemoryEntry {
                content,
                author: event.author.clone(),
                timestamp: event.timestamp,
            })
        })
        .collect();
    memory_service
        .add_session(state.app_name(), &input.user_id, &input.session_id, entries)
        .await?;
    Ok(Value::Null)
}

/// Handles `async_search_memory`.
pub(crate) async fn handle_search_memory(
    state: &AgentEngineState,
    input: SearchMemoryInput,
) -> Result<Value> {
    let Some(memory_service) = state.memory_service() else {
        return Err(memory_unavailable());
    };
    let response = memory_service
        .search(adk_memory::SearchRequest {
            query: input.query,
            user_id: input.user_id,
            app_name: state.app_name().to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await?;
    let memories: Vec<Value> = response
        .memories
        .into_iter()
        .map(|entry| {
            json!({
                "content": entry.content,
                "author": entry.author,
                "timestamp": entry.timestamp,
            })
        })
        .collect();
    Ok(json!({ "memories": memories }))
}

/// Resolves the message value of `stream_query` into a [`Content`].
///
/// A JSON string becomes a single-part user message; an object is
/// deserialized as a full `Content`, matching adk-python's `str | dict`.
pub(crate) fn message_to_content(message: Value) -> Result<Content> {
    match message {
        Value::String(text) => Ok(Content::new("user").with_text(text)),
        Value::Object(_) => serde_json::from_value(message).map_err(|err| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "agent_engine.invalid_message",
                format!("message object is not a valid Content: {err}"),
            )
        }),
        other => Err(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "agent_engine.invalid_message",
            format!("message must be a string or a Content object, got {other}"),
        )),
    }
}

/// Returns the session ID to run in, creating the session when it does not
/// exist (or when no ID was supplied), and whether it was created.
///
/// Matches `AdkApp.stream_query`: a supplied ID that has no session record is
/// created with that ID rather than rejected.
pub(crate) async fn resolve_session(
    state: &AgentEngineState,
    user_id: &str,
    session_id: Option<String>,
    initial_state: HashMap<String, Value>,
) -> Result<(String, bool)> {
    let session_id = match session_id {
        Some(id) => id,
        None => uuid::Uuid::new_v4().to_string(),
    };
    let existing = state
        .session_service()
        .get(adk_session::GetRequest {
            app_name: state.app_name().to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await;
    match existing {
        Ok(_) => Ok((session_id, false)),
        Err(err) if err.is_not_found() => {
            state
                .session_service()
                .create(adk_session::CreateRequest {
                    app_name: state.app_name().to_string(),
                    user_id: user_id.to_string(),
                    session_id: Some(session_id.clone()),
                    state: initial_state,
                })
                .await?;
            Ok((session_id, true))
        }
        Err(err) => Err(err),
    }
}

/// Applies a state delta to an existing session by appending a
/// state-delta-only event, mirroring the REST runtime's behavior.
pub(crate) async fn apply_state_delta(
    state: &AgentEngineState,
    user_id: &str,
    session_id: &str,
    state_delta: HashMap<String, Value>,
) -> Result<()> {
    if state_delta.is_empty() {
        return Ok(());
    }
    let identity = adk_core::AdkIdentity {
        app_name: adk_core::AppName::try_from(state.app_name())?,
        user_id: adk_core::UserId::try_from(user_id)?,
        session_id: adk_core::SessionId::try_from(session_id)?,
    };
    let mut event = adk_core::Event::new(format!("agent-engine-input-{}", uuid::Uuid::new_v4()));
    event.author = "agent_engine_dispatch".to_string();
    event.actions.state_delta = state_delta;
    state
        .session_service()
        .append_event_for_identity(adk_session::AppendEventRequest { identity, event })
        .await
}

/// Validates and types the identity pair for a runner invocation.
pub(crate) fn typed_identity(user_id: &str, session_id: &str) -> Result<(UserId, SessionId)> {
    let user_id = UserId::try_from(user_id)?;
    let session_id = SessionId::try_from(session_id)?;
    Ok((user_id, session_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_method_round_trips_through_from_str() {
        for method in ClassMethod::ALL {
            assert_eq!(ClassMethod::from_str(method.as_str()).unwrap(), method);
        }
    }

    #[test]
    fn unknown_class_method_is_invalid_input() {
        let err = ClassMethod::from_str("does_not_exist").unwrap_err();
        assert_eq!(err.http_status_code(), 400);
    }

    #[test]
    fn register_operations_covers_every_variant_exactly_once() {
        let map = handle_register_operations();
        let advertised: Vec<&str> = map
            .as_object()
            .unwrap()
            .values()
            .flat_map(|names| names.as_array().unwrap())
            .map(|name| name.as_str().unwrap())
            .collect();
        let expected: Vec<&str> = ClassMethod::ALL.iter().map(ClassMethod::as_str).collect();
        // Same multiset: every variant advertised exactly once.
        assert_eq!(advertised.len(), expected.len());
        for name in expected {
            assert_eq!(advertised.iter().filter(|n| **n == name).count(), 1, "{name}");
        }
    }

    #[test]
    fn api_modes_partition_the_streaming_endpoints() {
        for method in ClassMethod::ALL {
            let streaming = matches!(
                method,
                ClassMethod::StreamQuery
                    | ClassMethod::AsyncStreamQuery
                    | ClassMethod::StreamingAgentRunWithEvents
            );
            assert_eq!(method.api_mode().is_streaming(), streaming, "{}", method.as_str());
        }
    }

    #[test]
    fn agent_run_request_accepts_both_casings() {
        let camel: AgentRunRequest = serde_json::from_str(
            r#"{"appName":"a","userId":"u","sessionId":"s","newMessage":{"role":"user","parts":[{"text":"hi"}]}}"#,
        )
        .unwrap();
        let snake: AgentRunRequest = serde_json::from_str(
            r#"{"app_name":"a","user_id":"u","session_id":"s","new_message":{"role":"user","parts":[{"text":"hi"}]}}"#,
        )
        .unwrap();
        assert_eq!(camel.user_id, snake.user_id);
        assert_eq!(camel.session_id, snake.session_id);
        assert_eq!(
            serde_json::to_value(&camel.new_message).unwrap(),
            serde_json::to_value(&snake.new_message).unwrap()
        );
    }
}
