//! The A2A binding of the edge (spec 3.5 §7.4, §7.9; AWP 2.0 §7.4.2).
//!
//! | Route | Answers |
//! |---|---|
//! | `GET /.well-known/agent-card.json` | The A2A v1.0 Agent Card, one skill per capability the caller's tier sees, signed with the edge's delegated `card` key |
//! | `POST /awp/v2/a2a` | A2A v1.0 JSON-RPC: `SendMessage`, `GetTask`, `CancelTask` |
//! | `POST /awp/v2/a2a/v1/message:send`, `GET /awp/v2/a2a/v1/tasks/{id}`, `POST /awp/v2/a2a/v1/tasks/{id}/cancel` | The same over the A2A HTTP+JSON binding |
//!
//! A message whose `metadata` maps [`NS_CAPABILITY`] to a capability name invokes it with its
//! one data part as input; the task completes with the output as a data part, stays
//! `TASK_STATE_WORKING` with the held-call document while the owner decides, and ends
//! `TASK_STATE_REJECTED` or `TASK_STATE_FAILED` with the error envelope. Any other message is
//! conversation with the site's public agent.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use a2a_protocol_types::jsonrpc::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcRequestId, JsonRpcSuccessResponse,
    JsonRpcVersion,
};
use a2a_protocol_types::params::{CancelTaskParams, MessageSendParams, TaskQueryParams};
use a2a_protocol_types::responses::SendMessageResponse;
use a2a_protocol_types::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentSkill, Artifact, ArtifactId,
    ContextId, Message, MessageId, MessageRole, Part, PartContent, Task, TaskId, TaskState,
    TaskStatus,
};
use adk_awp::signing::card::sign_agent_card;
use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::executor::{Binding, Invocation, InvocationOutcome};
use awp_types::{ErrorBody, ErrorCode, ErrorEnvelope, HeldCallState, TrustTier};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::Value;
use uuid::Uuid;

use super::control::EdgeControl;
use super::executor::{GovernedExecutor, KeySources};
use super::public::{LiveThreads, PublicPartition};
use super::tiers::EdgeCaller;
use crate::a2a::v1::error::A2aError;

/// The Agent Card's path (spec 3.5 §7.9).
pub const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
/// The A2A JSON-RPC endpoint.
pub const A2A_PATH: &str = "/awp/v2/a2a";
/// The base of the A2A HTTP+JSON endpoints.
pub const A2A_REST_PATH: &str = "/awp/v2/a2a/v1";
/// The AWP extension the Agent Card declares (AWP 2.0 §7.4.2).
pub const AWP_A2A_EXTENSION: &str = "https://agenticwebprotocol.com/ns/2.0/a2a";
/// Message metadata key naming the capability a message invokes (AWP 2.0 §7.4.2, C9).
pub const NS_CAPABILITY: &str = "https://agenticwebprotocol.com/ns/2.0/capability";
/// Message metadata key holding an explicit idempotency key (AWP 2.0 §7.7).
pub const NS_IDEMPOTENCY_KEY: &str = "https://agenticwebprotocol.com/ns/2.0/idempotencyKey";
/// Message metadata key listing consent receipt ids (AWP 2.0 §7.4.2, C12).
pub const NS_CONSENT: &str = "https://agenticwebprotocol.com/ns/2.0/consent";

/// The most tasks the edge keeps; the oldest are dropped first.
const MAX_TASKS: usize = 10_000;
/// The longest conversation message the edge passes to its public agent, in characters.
const MAX_MESSAGE_CHARS: usize = 8_000;

/// What the Agent Card says about the site's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCardInfo {
    /// The agent's name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// The card's version.
    pub version: String,
}

struct TaskEntry {
    owner: Option<String>,
    task: Task,
    held: Option<String>,
}

#[derive(Default)]
struct TaskBook {
    tasks: HashMap<String, TaskEntry>,
    order: VecDeque<String>,
    by_message: HashMap<(Option<String>, String), String>,
}

pub(crate) struct A2aBinding {
    executor: GovernedExecutor,
    public: Option<Arc<PublicPartition>>,
    live: LiveThreads,
    control: Arc<EdgeControl>,
    info: AgentCardInfo,
    a2a_url: String,
    rest_url: String,
    manifest_url: String,
    signer: Arc<dyn DocumentSigner>,
    cards: tokio::sync::Mutex<HashMap<TrustTier, Value>>,
    book: Mutex<TaskBook>,
}

impl A2aBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        executor: GovernedExecutor,
        public: Option<Arc<PublicPartition>>,
        live: LiveThreads,
        control: Arc<EdgeControl>,
        info: AgentCardInfo,
        base_url: &str,
        signer: Arc<dyn DocumentSigner>,
    ) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            executor,
            public,
            live,
            control,
            info,
            a2a_url: format!("{base}{A2A_PATH}"),
            rest_url: format!("{base}{A2A_REST_PATH}"),
            manifest_url: format!("{base}/awp/v2/manifest"),
            signer,
            cards: tokio::sync::Mutex::new(HashMap::new()),
            book: Mutex::new(TaskBook::default()),
        }
    }

    pub(crate) fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route(AGENT_CARD_PATH, get(agent_card))
            .route(A2A_PATH, post(jsonrpc))
            .route(&format!("{A2A_REST_PATH}/message:send"), post(rest_send))
            .route(&format!("{A2A_REST_PATH}/tasks/{{id}}"), get(rest_get))
            .route(&format!("{A2A_REST_PATH}/tasks/{{id}}/cancel"), post(rest_cancel))
            .with_state(self)
    }

    /// The signed card a caller of `tier` sees, signed once per tier.
    pub(crate) async fn card(&self, tier: TrustTier) -> Result<Value, String> {
        let mut cards = self.cards.lock().await;
        if let Some(card) = cards.get(&tier) {
            return Ok(card.clone());
        }
        let skills = self
            .executor
            .registry()
            .manifest_for(tier)
            .capabilities
            .into_iter()
            .map(|capability| AgentSkill {
                id: capability.name.clone(),
                name: capability.name.clone(),
                description: capability.description.clone(),
                tags: vec![
                    "awp".to_owned(),
                    serde_json::to_value(capability.effect)
                        .ok()
                        .and_then(|effect| effect.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                ],
                examples: None,
                input_modes: Some(vec!["application/json".to_owned()]),
                output_modes: Some(vec!["application/json".to_owned()]),
                security_requirements: None,
            })
            .collect();
        let mut capabilities = AgentCapabilities::none();
        capabilities.streaming = Some(false);
        capabilities.push_notifications = Some(false);
        capabilities.extensions = Some(vec![AgentExtension {
            uri: AWP_A2A_EXTENSION.to_owned(),
            description: Some(
                "AWP 2.0: capabilities as skills; schemas in the capability manifest".to_owned(),
            ),
            required: Some(false),
            params: Some(serde_json::json!({ "manifest": self.manifest_url })),
        }]);
        let interface = |url: &str, binding: &str| AgentInterface {
            url: url.to_owned(),
            protocol_binding: binding.to_owned(),
            protocol_version: "1.0".to_owned(),
            tenant: None,
        };
        let card = AgentCard {
            name: self.info.name.clone(),
            url: Some(self.a2a_url.clone()),
            description: self.info.description.clone(),
            version: self.info.version.clone(),
            supported_interfaces: vec![
                interface(&self.a2a_url, "JSONRPC"),
                interface(&self.rest_url, "HTTP+JSON"),
            ],
            default_input_modes: vec!["text/plain".to_owned(), "application/json".to_owned()],
            default_output_modes: vec!["text/plain".to_owned(), "application/json".to_owned()],
            skills,
            capabilities,
            provider: None,
            icon_url: None,
            documentation_url: None,
            security_schemes: None,
            security_requirements: None,
            signatures: None,
        };
        let value = serde_json::to_value(&card).map_err(|error| error.to_string())?;
        let signed = sign_agent_card(&value, self.signer.as_ref())
            .await
            .map_err(|error| error.to_string())?;
        cards.insert(tier, signed.clone());
        Ok(signed)
    }

    fn store(&self, entry: TaskEntry, message_key: Option<(Option<String>, String)>) {
        let mut book = self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = entry.task.id.0.clone();
        if let Some(key) = message_key {
            book.by_message.insert(key, id.clone());
        }
        book.order.push_back(id.clone());
        book.tasks.insert(id, entry);
        while book.order.len() > MAX_TASKS {
            if let Some(oldest) = book.order.pop_front() {
                book.tasks.remove(&oldest);
                book.by_message.retain(|_, task| *task != oldest);
            }
        }
    }

    /// The envelope of `error`, with the signed status for `read_only`.
    async fn envelope(&self, error: ErrorBody) -> Value {
        let mut envelope = ErrorEnvelope { awp: awp_types::VERSION_2_0, error, status: None };
        if envelope.error.code == ErrorCode::ReadOnly {
            envelope.status = self.control.current_status().await;
        }
        serde_json::to_value(envelope).unwrap_or(Value::Null)
    }

    fn agent_message(task_id: &TaskId, context_id: &ContextId, part: Part) -> Message {
        Message {
            id: MessageId::new(Uuid::new_v4().to_string()),
            role: MessageRole::Agent,
            parts: vec![part],
            task_id: Some(task_id.clone()),
            context_id: Some(context_id.clone()),
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    async fn failed_status(&self, task: &Task, error: ErrorBody) -> TaskStatus {
        // A refusal before execution is a rejection; a failure of the site is a failure.
        let state = if error.code.http_status() < 500 && error.code != ErrorCode::ReadOnly {
            TaskState::Rejected
        } else {
            TaskState::Failed
        };
        let mut status = TaskStatus::with_timestamp(state);
        status.message = Some(Self::agent_message(
            &task.id,
            &task.context_id,
            Part::data(self.envelope(error).await),
        ));
        status
    }

    /// Applies a capability outcome to `task`; returns the held call id it now waits on.
    async fn apply_outcome(&self, task: &mut Task, outcome: InvocationOutcome) -> Option<String> {
        match outcome {
            InvocationOutcome::Completed(output) => {
                task.status = TaskStatus::with_timestamp(TaskState::Completed);
                task.artifacts = Some(vec![Artifact::new(
                    ArtifactId::new(Uuid::new_v4().to_string()),
                    vec![Part::data(output)],
                )]);
                None
            }
            InvocationOutcome::Held(call) => {
                let id = call.id.clone();
                let mut status = TaskStatus::with_timestamp(TaskState::Working);
                status.message = Some(Self::agent_message(
                    &task.id,
                    &task.context_id,
                    Part::data(serde_json::to_value(&call).unwrap_or(Value::Null)),
                ));
                task.status = status;
                Some(id)
            }
            InvocationOutcome::Failed(error) => {
                task.status = self.failed_status(task, error).await;
                None
            }
            _ => {
                let error = ErrorBody::new(ErrorCode::InternalError, "the call did not complete");
                task.status = self.failed_status(task, error).await;
                None
            }
        }
    }

    fn new_task(context_id: ContextId, message: &Message) -> Task {
        Task {
            id: TaskId::new(Uuid::new_v4().to_string()),
            context_id,
            status: TaskStatus::with_timestamp(TaskState::Working),
            history: Some(vec![message.clone()]),
            artifacts: None,
            metadata: None,
        }
    }

    pub(crate) async fn send(
        &self,
        caller: &EdgeCaller,
        message: Message,
    ) -> Result<Task, A2aError> {
        if message.parts.is_empty() {
            return Err(A2aError::InvalidParams {
                message: "message must contain at least one part".to_owned(),
            });
        }
        if message.id.0.is_empty() || message.id.0.len() > 256 {
            return Err(A2aError::InvalidParams {
                message: "messageId must hold 1 to 256 characters".to_owned(),
            });
        }
        let owner = caller.requester.as_ref().map(|requester| requester.as_str().to_owned());
        let message_key = (owner.clone(), message.id.0.clone());
        let existing = {
            let book = self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            book.by_message.get(&message_key).cloned()
        };
        if let Some(task_id) = existing
            && let Ok(task) = self.get(caller, &task_id).await
        {
            return Ok(task);
        }
        let metadata = message.metadata.clone().unwrap_or(Value::Null);
        let context_id = message
            .context_id
            .clone()
            .unwrap_or_else(|| ContextId::new(Uuid::new_v4().to_string()));
        let consent: Vec<Uuid> = metadata
            .get(NS_CONSENT)
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(|id| id.as_str()?.parse().ok()).collect())
            .unwrap_or_default();
        let mut task = Self::new_task(context_id.clone(), &message);
        let held = match metadata.get(NS_CAPABILITY).and_then(Value::as_str) {
            Some(capability) => {
                let input = message
                    .parts
                    .iter()
                    .find_map(|part| match &part.content {
                        PartContent::Data(data) => Some(data.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| serde_json::json!({}));
                let source = self
                    .executor
                    .registry()
                    .get(capability)
                    .map(|tool| tool.declaration().idempotency)
                    .unwrap_or_default();
                let keys = KeySources {
                    explicit: metadata
                        .get(NS_IDEMPOTENCY_KEY)
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    binding_id: Some(message.id.0.clone()),
                };
                let invocation = Invocation {
                    capability: capability.to_owned(),
                    input,
                    tier: caller.tier,
                    requester: caller.requester.clone(),
                    idempotency_key: keys.key_for(source, Binding::A2a),
                    consent,
                    binding: Binding::A2a,
                };
                let outcome = self.executor.invoke_as(caller, invocation).await;
                self.apply_outcome(&mut task, outcome).await
            }
            None => {
                self.converse(caller, &mut task, &message, &consent).await;
                None
            }
        };
        self.store(TaskEntry { owner, task: task.clone(), held }, Some(message_key));
        Ok(task)
    }

    async fn converse(
        &self,
        caller: &EdgeCaller,
        task: &mut Task,
        message: &Message,
        consent: &[Uuid],
    ) {
        let Some(public) = &self.public else {
            let error = ErrorBody::new(
                ErrorCode::CapabilityNotFound,
                "this site holds no conversation over A2A; name a capability",
            );
            task.status = self.failed_status(task, error).await;
            return;
        };
        if self.control.is_read_only() {
            let mut error = ErrorBody::new(ErrorCode::ReadOnly, "the site is read-only");
            error.retryable = true;
            task.status = self.failed_status(task, error).await;
            return;
        }
        let text: String = message
            .texts()
            .collect::<Vec<_>>()
            .join("\n")
            .chars()
            .take(MAX_MESSAGE_CHARS)
            .collect();
        let governance = self.control.governance().cloned();
        // In-thread replies during the turn answer this task, before the agent's last word.
        let thread = format!("a2a-{}", task.id.0);
        let (sender, mut replies) = tokio::sync::mpsc::channel::<String>(16);
        self.live.open(&thread, sender);
        let answer = public
            .converse(caller, "a2a", &task.context_id.0, &thread, &text, consent, governance)
            .await;
        self.live.close(&thread);
        let mut parts = Vec::new();
        while let Ok(reply) = replies.try_recv() {
            parts.push(Part::text(reply));
        }
        match answer {
            Ok(reply) => {
                if !reply.is_empty() {
                    parts.push(Part::text(reply));
                }
                task.status = TaskStatus::with_timestamp(TaskState::Completed);
                task.artifacts = Some(
                    parts
                        .into_iter()
                        .map(|part| {
                            Artifact::new(ArtifactId::new(Uuid::new_v4().to_string()), vec![part])
                        })
                        .collect(),
                );
            }
            Err(error) => {
                tracing::warn!(error = %error, "awp public conversation failed");
                let body = ErrorBody::new(ErrorCode::InternalError, "the conversation failed");
                task.status = self.failed_status(task, body).await;
            }
        }
    }

    pub(crate) async fn get(&self, caller: &EdgeCaller, id: &str) -> Result<Task, A2aError> {
        let owner = caller.requester.as_ref().map(|requester| requester.as_str().to_owned());
        let (mut task, held) = {
            let book = self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match book.tasks.get(id) {
                Some(entry) if entry.owner.is_none() || entry.owner == owner => {
                    (entry.task.clone(), entry.held.clone())
                }
                _ => return Err(A2aError::TaskNotFound { task_id: id.to_owned() }),
            }
        };
        let Some(held_id) = held else { return Ok(task) };
        let call = match self.executor.resolve(&held_id).await {
            Ok(Some(call)) => call,
            Ok(None) => return Ok(task),
            Err(error) => return Err(A2aError::Internal { message: error.to_string() }),
        };
        let still_held = match call.state {
            HeldCallState::Pending | HeldCallState::Approved => return Ok(task),
            HeldCallState::Completed => match (call.result, call.error) {
                (Some(result), _) => {
                    self.apply_outcome(&mut task, InvocationOutcome::Completed(result)).await
                }
                (None, Some(error)) => {
                    task.status = TaskStatus::with_timestamp(TaskState::Failed);
                    task.status.message = Some(Self::agent_message(
                        &task.id,
                        &task.context_id,
                        Part::data(self.envelope(error).await),
                    ));
                    None
                }
                (None, None) => None,
            },
            HeldCallState::Rejected => {
                let error = call.error.unwrap_or_else(|| {
                    ErrorBody::new(ErrorCode::Forbidden, "the owner declined the call")
                });
                task.status = TaskStatus::with_timestamp(TaskState::Rejected);
                task.status.message = Some(Self::agent_message(
                    &task.id,
                    &task.context_id,
                    Part::data(self.envelope(error).await),
                ));
                None
            }
            _ => {
                let error = call.error.unwrap_or_else(|| {
                    ErrorBody::new(ErrorCode::Forbidden, "the owner did not decide in time")
                });
                task.status = TaskStatus::with_timestamp(TaskState::Failed);
                task.status.message = Some(Self::agent_message(
                    &task.id,
                    &task.context_id,
                    Part::data(self.envelope(error).await),
                ));
                None
            }
        };
        let mut book = self.book.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = book.tasks.get_mut(id) {
            entry.task = task.clone();
            entry.held = still_held;
        }
        Ok(task)
    }

    pub(crate) async fn cancel(&self, caller: &EdgeCaller, id: &str) -> Result<Task, A2aError> {
        let task = self.get(caller, id).await?;
        // A held call is the owner's to decide; a finished task cannot be cancelled.
        Err(A2aError::TaskNotCancelable {
            task_id: task.id.0,
            current_state: format!("{:?}", task.status.state),
        })
    }
}

fn caller_of(caller: Option<Extension<EdgeCaller>>) -> EdgeCaller {
    caller.map_or_else(|| EdgeCaller::public("ip:unknown"), |Extension(caller)| caller)
}

async fn agent_card(
    State(binding): State<Arc<A2aBinding>>,
    caller: Option<Extension<EdgeCaller>>,
) -> Response {
    let caller = caller_of(caller);
    match binding.card(caller.tier).await {
        Ok(card) => {
            let mut response = Json(card).into_response();
            response.headers_mut().insert(
                header::VARY,
                HeaderValue::from_static("Signature, Signature-Input, Authorization"),
            );
            response
        }
        Err(error) => {
            tracing::error!(error = %error, "awp agent card could not be signed");
            super::envelope(
                ErrorBody::new(ErrorCode::InternalError, "the agent card is unavailable"),
                None,
            )
        }
    }
}

fn rpc_error(id: &JsonRpcRequestId, error: &A2aError) -> Response {
    let body = JsonRpcErrorResponse::new(
        id.to_response_id(),
        JsonRpcError::with_data(error.json_rpc_code(), error.to_string(), error.to_error_info()),
    );
    a2a_json(serde_json::to_value(body).unwrap_or(Value::Null))
}

fn rpc_result<T: serde::Serialize>(id: &JsonRpcRequestId, result: &T) -> Response {
    let body = JsonRpcSuccessResponse { jsonrpc: JsonRpcVersion, id: id.to_response_id(), result };
    a2a_json(serde_json::to_value(body).unwrap_or(Value::Null))
}

fn a2a_json(value: Value) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/a2a+json"));
    response
}

fn params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, A2aError> {
    serde_json::from_value(params.unwrap_or(Value::Null))
        .map_err(|error| A2aError::InvalidParams { message: error.to_string() })
}

async fn jsonrpc(
    State(binding): State<Arc<A2aBinding>>,
    caller: Option<Extension<EdgeCaller>>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    let caller = caller_of(caller);
    let id = request.id.clone();
    let result = match request.method.as_str() {
        "SendMessage" | "message/send" => match params::<MessageSendParams>(request.params) {
            Ok(params) => binding.send(&caller, params.message).await.map(|task| {
                serde_json::to_value(SendMessageResponse::Task(task)).unwrap_or(Value::Null)
            }),
            Err(error) => Err(error),
        },
        "GetTask" | "tasks/get" => match params::<TaskQueryParams>(request.params) {
            Ok(params) => binding
                .get(&caller, &params.id)
                .await
                .map(|task| serde_json::to_value(task).unwrap_or(Value::Null)),
            Err(error) => Err(error),
        },
        "CancelTask" | "tasks/cancel" => match params::<CancelTaskParams>(request.params) {
            Ok(params) => binding
                .cancel(&caller, &params.id)
                .await
                .map(|task| serde_json::to_value(task).unwrap_or(Value::Null)),
            Err(error) => Err(error),
        },
        "SendStreamingMessage"
        | "SubscribeToTask"
        | "ListTasks"
        | "GetExtendedAgentCard"
        | "CreateTaskPushNotificationConfig"
        | "GetTaskPushNotificationConfig"
        | "ListTaskPushNotificationConfigs"
        | "DeleteTaskPushNotificationConfig" => {
            Err(A2aError::UnsupportedOperation { method: request.method.clone() })
        }
        other => Err(A2aError::MethodNotFound { method: other.to_owned() }),
    };
    match result {
        Ok(value) => rpc_result(&id, &value),
        Err(error) => rpc_error(&id, &error),
    }
}

fn rest_error(error: &A2aError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_http_error_response())).into_response()
}

async fn rest_send(
    State(binding): State<Arc<A2aBinding>>,
    caller: Option<Extension<EdgeCaller>>,
    Json(params): Json<MessageSendParams>,
) -> Response {
    match binding.send(&caller_of(caller), params.message).await {
        Ok(task) => {
            a2a_json(serde_json::to_value(SendMessageResponse::Task(task)).unwrap_or(Value::Null))
        }
        Err(error) => rest_error(&error),
    }
}

async fn rest_get(
    State(binding): State<Arc<A2aBinding>>,
    caller: Option<Extension<EdgeCaller>>,
    Path(id): Path<String>,
) -> Response {
    match binding.get(&caller_of(caller), &id).await {
        Ok(task) => a2a_json(serde_json::to_value(task).unwrap_or(Value::Null)),
        Err(error) => rest_error(&error),
    }
}

async fn rest_cancel(
    State(binding): State<Arc<A2aBinding>>,
    caller: Option<Extension<EdgeCaller>>,
    Path(id): Path<String>,
) -> Response {
    match binding.cancel(&caller_of(caller), &id).await {
        Ok(task) => a2a_json(serde_json::to_value(task).unwrap_or(Value::Null)),
        Err(error) => rest_error(&error),
    }
}
