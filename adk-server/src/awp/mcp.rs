//! The MCP binding of the edge (spec 3.5 §7.4, §7.9; AWP 2.0 §7.4.3).
//!
//! The capabilities are served as MCP tools by `adk-tool`'s MCP server at `/mcp`, with its
//! Server Card at `/mcp/server-card` and `/.well-known/mcp/server-card.json`. Every
//! `tools/call` goes through the edge's governed path for the caller the edge assessed;
//! `tools/list` shows only the capabilities the caller's tier sees.
//!
//! | AWP element | MCP |
//! |---|---|
//! | Idempotency key | `params._meta["agenticwebprotocol.com/idempotency-key"]`, else the JSON-RPC id within its session |
//! | Consent | `params._meta["agenticwebprotocol.com/consent"]`, an array of receipt ids |
//! | Output | `structuredContent`, with a text block holding its JSON |
//! | Held call | `structuredContent` is the held-call document; `isError` is false |
//! | Error | `isError` is true; the text block carries the error envelope's JSON |

use std::collections::HashMap;
use std::sync::Arc;

use adk_awp::v2::executor::{Binding, Invocation, InvocationOutcome};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, Url};
use adk_tool::mcp::server::{
    McpCallExecutor, McpCaller, McpCallerResolver, McpHttpRequest, McpRouterBuilder, McpServer,
    McpSessions, McpToolCall, McpToolOutcome,
};
use async_trait::async_trait;
use awp_types::{ErrorBody, ErrorCode, ErrorEnvelope};
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use uuid::Uuid;

use super::control::EdgeControl;
use super::executor::{GovernedExecutor, KeySources};
use super::tiers::EdgeCaller;

/// The MCP endpoint's path.
pub const MCP_PATH: &str = "/mcp";
/// The `_meta` key carrying an idempotency key (AWP 2.0 §7.4.3, C8).
pub const META_IDEMPOTENCY_KEY: &str = "agenticwebprotocol.com/idempotency-key";
/// The `_meta` key listing consent receipt ids (AWP 2.0 §7.4.3, C12).
pub const META_CONSENT_IDS: &str = "agenticwebprotocol.com/consent";

/// The largest MCP request body the edge reads.
const MAX_MCP_BODY: usize = 1024 * 1024;

/// The AWP `_meta` members of one `tools/call`, by JSON-RPC request id.
#[derive(Debug, Clone, Default)]
struct CallMetas(HashMap<String, (Option<String>, Vec<Uuid>)>);

fn collect_meta(message: &Value, metas: &mut CallMetas) {
    if message.get("method").and_then(Value::as_str) != Some("tools/call") {
        return;
    }
    let id = match message.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => return,
    };
    let meta = message.pointer("/params/_meta");
    let key = meta
        .and_then(|meta| meta.get(META_IDEMPOTENCY_KEY))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let consent = meta
        .and_then(|meta| meta.get(META_CONSENT_IDS))
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(|id| id.as_str()?.parse().ok()).collect())
        .unwrap_or_default();
    metas.0.insert(id, (key, consent));
}

/// Reads the AWP `_meta` members of `tools/call` requests before the MCP server parses them.
async fn capture_meta(request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_MCP_BODY).await else {
        return (StatusCode::PAYLOAD_TOO_LARGE, "the MCP request is too large").into_response();
    };
    let mut metas = CallMetas::default();
    if let Ok(message) = serde_json::from_slice::<Value>(&bytes) {
        match &message {
            Value::Array(batch) => batch.iter().for_each(|item| collect_meta(item, &mut metas)),
            single => collect_meta(single, &mut metas),
        }
    }
    parts.extensions.insert(metas);
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

/// Attaches the edge's assessment of the request to the MCP caller.
struct EdgeCallers;

#[async_trait]
impl McpCallerResolver for EdgeCallers {
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> Result<McpCaller> {
        let caller = request
            .extensions()
            .get::<EdgeCaller>()
            .cloned()
            .unwrap_or_else(|| EdgeCaller::public("ip:unknown"));
        let metas = request.extensions().get::<CallMetas>().cloned().unwrap_or_default();
        let mcp = match &caller.requester {
            Some(requester) => McpCaller::authenticated(requester.as_str()),
            None => McpCaller::anonymous(),
        };
        Ok(mcp.with_extension(caller).with_extension(metas))
    }
}

/// Routes every `tools/call` through the edge's governed path.
struct EdgeMcpExecutor {
    executor: GovernedExecutor,
    control: Arc<EdgeControl>,
}

fn category_for(code: &ErrorCode) -> ErrorCategory {
    // Rejection categories, so the MCP server shows the envelope to the client.
    match code.http_status() {
        404 => ErrorCategory::NotFound,
        409 | 410 => ErrorCategory::Conflict,
        401..=403 | 451 => ErrorCategory::Forbidden,
        429 => ErrorCategory::ResourceExhausted,
        _ => ErrorCategory::InvalidInput,
    }
}

impl EdgeMcpExecutor {
    async fn failed(&self, error: ErrorBody) -> McpToolOutcome {
        let mut envelope = ErrorEnvelope { awp: awp_types::VERSION_2_0, error, status: None };
        if envelope.error.code == ErrorCode::ReadOnly {
            envelope.status = self.control.current_status().await;
        }
        let category = category_for(&envelope.error.code);
        let mut text = serde_json::to_string(&envelope).unwrap_or_default();
        // The MCP server shows at most this much of an error; the status stays at its URL.
        if text.chars().count() > adk_core::TOOL_ERROR_MESSAGE_LIMIT {
            envelope.status = None;
            text = serde_json::to_string(&envelope).unwrap_or_default();
        }
        McpToolOutcome::Failed(AdkError::new(ErrorComponent::Tool, category, "awp.error", text))
    }
}

#[async_trait]
impl McpCallExecutor for EdgeMcpExecutor {
    async fn execute(&self, call: McpToolCall) -> Result<McpToolOutcome> {
        let caller = call
            .caller
            .extension::<EdgeCaller>()
            .cloned()
            .unwrap_or_else(|| EdgeCaller::public("ip:unknown"));
        let (explicit, consent) = call
            .caller
            .extension::<CallMetas>()
            .and_then(|metas| metas.0.get(&call.request_id).cloned())
            .unwrap_or_default();
        let name = call.tool.name().to_owned();
        let source = self
            .executor
            .registry()
            .get(&name)
            .map(|tool| tool.declaration().idempotency)
            .unwrap_or_default();
        // A JSON-RPC id is unique only within its session (AWP 2.0 §7.7, C8).
        let binding_id = match &call.session_id {
            Some(session) => format!("{session}:{}", call.request_id),
            None => call.request_id.clone(),
        };
        let keys = KeySources { explicit, binding_id: Some(binding_id) };
        let invocation = Invocation {
            capability: name,
            input: call.arguments,
            tier: caller.tier,
            requester: caller.requester.clone(),
            idempotency_key: keys.key_for(source, Binding::Mcp),
            consent,
            binding: Binding::Mcp,
        };
        Ok(match self.executor.invoke_as(&caller, invocation).await {
            InvocationOutcome::Completed(output) => McpToolOutcome::Completed(output),
            InvocationOutcome::Held(held) => {
                McpToolOutcome::Completed(serde_json::to_value(held).unwrap_or(Value::Null))
            }
            InvocationOutcome::Failed(error) => self.failed(error).await,
            _ => {
                self.failed(ErrorBody::new(ErrorCode::InternalError, "the call did not complete"))
                    .await
            }
        })
    }

    fn is_visible(&self, tool: &dyn Tool, caller: &McpCaller) -> bool {
        let tier = caller
            .extension::<EdgeCaller>()
            .map_or(awp_types::TrustTier::Public, |caller| caller.tier);
        self.executor.registry().visible(tool.name(), tier).is_some()
    }
}

/// How the MCP server describes itself.
pub(crate) struct McpInfo<'a> {
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
    pub(crate) version: &'a str,
}

/// The edge's MCP server and its routes.
///
/// # Errors
///
/// An `adk_core::AdkError` when the server cannot be built.
pub(crate) fn router(
    executor: &GovernedExecutor,
    control: Arc<EdgeControl>,
    info: McpInfo<'_>,
    base_url: &Url,
    sessions: McpSessions,
) -> Result<Router> {
    let server = McpServer::builder(info.name, info.version)
        .description(info.description)
        .tools(executor.registry().tools())
        .open_world(false)
        .executor(Arc::new(EdgeMcpExecutor { executor: executor.clone(), control }))
        .build()?;
    let public_url = base_url.join(MCP_PATH.trim_start_matches('/')).map_err(|error| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "awp.edge.url",
            error.to_string(),
        )
    })?;
    let router = McpRouterBuilder::new(server)
        .path(MCP_PATH)
        .public_url(public_url)
        .caller_resolver(Arc::new(EdgeCallers))
        .sessions(sessions)
        // The server's host guard applies through `add_root_routes`.
        .allowed_hosts(Vec::<String>::new())
        .build()?;
    Ok(router.layer(from_fn(capture_meta)))
}
