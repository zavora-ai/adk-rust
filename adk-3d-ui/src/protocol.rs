use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type SessionId = String;
pub type Seq = u64;
pub type UiId = String;
pub type UiProps = Map<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SseEnvelope<T> {
    pub seq: Seq,
    pub session: SessionId,
    pub ts: String,
    pub payload: T,
}

impl<T> SseEnvelope<T> {
    pub fn new(seq: Seq, session: SessionId, payload: T) -> Self {
        Self {
            seq,
            session,
            ts: Utc::now().to_rfc3339(),
            payload,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SsePayload {
    UiOps(UiOpsPayload),
    Toast(ToastPayload),
    Done(DonePayload),
    Error(ErrorPayload),
    Log(LogPayload),
    Ping(PingPayload),
}

impl SsePayload {
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::UiOps(_) => "ui_ops",
            Self::Toast(_) => "toast",
            Self::Done(_) => "done",
            Self::Error(_) => "error",
            Self::Log(_) => "log",
            Self::Ping(_) => "ping",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiOpsPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Seq>,
    pub ops: Vec<UiOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToastPayload {
    pub level: ToastLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DonePayload {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPayload {
    pub level: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingPayload {
    pub ts: String,
}

impl PingPayload {
    pub fn now() -> Self {
        Self {
            ts: Utc::now().to_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UiOp {
    Create(UiCreateOp),
    Patch(UiPatchOp),
    Remove(UiRemoveOp),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCreateOp {
    pub id: UiId,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<UiId>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub props: UiProps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiPatchOp {
    pub id: UiId,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub props: UiProps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiRemoveOp {
    pub id: UiId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiEventRequest {
    pub seq: Seq,
    pub event: UiEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Select { id: UiId },
    Command { text: String },
    ApproveAction { action_id: String, approved: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiEventAck {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_seq: Option<Seq>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPromptRequest {
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunPromptResponse {
    pub accepted: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateResponse {
    pub session_id: SessionId,
}
