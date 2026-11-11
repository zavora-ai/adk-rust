//! Tools of the MCP servers an ACP client serves over the ACP connection.
//!
//! With the unstable MCP-over-ACP surface a client declares an `acp` MCP
//! server (`McpServer::Acp`: a name and an opaque `serverId`) in a session
//! setup request, and the agent reaches it by sending `mcp/message` requests
//! back to the client. [`AcpMcpToolset`] makes such a server's tools part of
//! the session's toolsets, next to the stdio MCP servers the session attached.
//!
//! The inner protocol is MCP `2026-07-28`: no `initialize` handshake, and every
//! request carries its protocol version and client capabilities in
//! `params._meta`. Each operation gets a fresh logical request id. Tool
//! results map to ADK-Rust values the way adk-tool's MCP toolset maps them, so
//! the agent sees the same shapes whichever transport served a tool.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use adk_core::{AdkError, ReadonlyContext, Result, Tool, ToolContext, ToolEffect, Toolset};
use agent_client_protocol::Client;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{McpServerAcpId, MessageMcpRequest, MessageMcpResponse};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};
use tracing::debug;

use crate::mcp::{MCP_CLIENT_CAPABILITIES_META, MCP_PROTOCOL_VERSION, MCP_PROTOCOL_VERSION_META};

/// The most `tools/list` pages followed for one listing.
const MAX_LIST_PAGES: usize = 64;

/// Most UTF-8 bytes of an `isError` result's text passed to the model.
const MAX_SERVER_ERROR_BYTES: usize = 8 * 1024;

/// The tools of one `acp` MCP server a client declared for a session.
pub(crate) struct AcpMcpToolset {
    inner: Arc<Remote>,
}

/// What a toolset and its tools share: the client connection and the server's
/// declared identity.
struct Remote {
    /// The toolset name, `acp:<server name>`, which also names the tools'
    /// origin.
    name: String,
    origin: String,
    server_name: String,
    server_id: McpServerAcpId,
    connection: ConnectionTo<Client>,
    /// The last listing, kept while its `ttlMs` lasts.
    cache: Mutex<Option<CachedTools>>,
}

/// A tool listing and the instant it stops being fresh.
type CachedTools = (Instant, Vec<Arc<dyn Tool>>);

impl AcpMcpToolset {
    pub(crate) fn new(
        server_name: &str,
        server_id: McpServerAcpId,
        connection: ConnectionTo<Client>,
    ) -> Self {
        let name = format!("acp:{server_name}");
        Self {
            inner: Arc::new(Remote {
                origin: format!("mcp:{name}"),
                name,
                server_name: server_name.to_string(),
                server_id,
                connection,
                cache: Mutex::new(None),
            }),
        }
    }
}

#[async_trait]
impl Toolset for AcpMcpToolset {
    fn name(&self) -> &str {
        &self.inner.name
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Remote::tools(&self.inner).await
    }
}

impl Remote {
    async fn tools(this: &Arc<Self>) -> Result<Vec<Arc<dyn Tool>>> {
        let cached = {
            let cache = this.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .as_ref()
                .filter(|(expires, _)| Instant::now() < *expires)
                .map(|(_, tools)| tools.clone())
        };
        if let Some(tools) = cached {
            return Ok(tools);
        }

        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut ttl = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut params = Map::new();
            if let Some(cursor) = cursor.take() {
                params.insert("cursor".into(), Value::String(cursor));
            }
            let page = this.request("tools/list", params).await?;
            ttl = ttl.or_else(|| page.get("ttlMs").and_then(Value::as_u64));
            let listed = page.get("tools").and_then(Value::as_array).ok_or_else(|| {
                AdkError::tool(format!(
                    "MCP server '{}' answered tools/list without a tools array",
                    this.server_name
                ))
            })?;
            for declaration in listed {
                tools.push(Arc::new(RemoteTool::from_declaration(this, declaration)?));
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => {
                    if let Some(ttl) = ttl.filter(|ttl| *ttl > 0) {
                        *this.cache.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some((Instant::now() + Duration::from_millis(ttl), tools.clone()));
                    }
                    return Ok(tools);
                }
            }
        }
        Err(AdkError::tool(format!(
            "MCP server '{}' returned more than {MAX_LIST_PAGES} tools/list pages",
            this.server_name
        )))
    }

    /// Send one MCP request to the client's server and return its result.
    ///
    /// An MCP error, a non-object result and a failed ACP request are errors.
    async fn request(&self, method: &str, mut params: Map<String, Value>) -> Result<Value> {
        params.insert(
            "_meta".into(),
            json!({
                MCP_PROTOCOL_VERSION_META: MCP_PROTOCOL_VERSION,
                MCP_CLIENT_CAPABILITIES_META: {},
            }),
        );
        let request_id = format!("adk-acp:{}", uuid::Uuid::new_v4());
        let request =
            MessageMcpRequest::new(self.server_id.clone(), request_id, method).params(params);
        let response =
            self.connection.send_request(request).block_task().await.map_err(|error| {
                AdkError::tool(format!(
                    "MCP server '{}' could not be reached over ACP for {method}: {error}",
                    self.server_name
                ))
            })?;
        match response {
            MessageMcpResponse::Result { result: result @ Value::Object(_), .. } => Ok(result),
            MessageMcpResponse::Result { .. } => Err(AdkError::tool(format!(
                "MCP server '{}' answered {method} with a result that is not an object",
                self.server_name
            ))),
            MessageMcpResponse::Error { error, .. } => Err(AdkError::tool(format!(
                "MCP server '{}' answered {method} with error {}: {}",
                self.server_name, error.code, error.message
            ))),
            other => Err(AdkError::tool(format!(
                "MCP server '{}' answered {method} with an unknown response: {other:?}",
                self.server_name
            ))),
        }
    }
}

/// One tool of a client's `acp` MCP server.
struct RemoteTool {
    remote: Arc<Remote>,
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Option<Value>,
    annotations: Option<Value>,
}

impl RemoteTool {
    fn from_declaration(remote: &Arc<Remote>, declaration: &Value) -> Result<Self> {
        let name = declaration.get("name").and_then(Value::as_str).filter(|name| !name.is_empty());
        let Some(name) = name else {
            return Err(AdkError::tool(format!(
                "MCP server '{}' listed a tool without a name",
                remote.server_name
            )));
        };
        let input_schema = match declaration.get("inputSchema") {
            Some(schema @ Value::Object(_)) => schema.clone(),
            _ => json!({ "type": "object" }),
        };
        Ok(Self {
            remote: Arc::clone(remote),
            name: name.to_string(),
            description: declaration
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_schema,
            output_schema: declaration
                .get("outputSchema")
                .filter(|schema| schema.is_object())
                .cloned(),
            annotations: declaration.get("annotations").filter(|value| value.is_object()).cloned(),
        })
    }

    fn hint(&self, name: &str) -> Option<bool> {
        self.annotations.as_ref()?.get(name)?.as_bool()
    }
}

#[async_trait]
impl Tool for RemoteTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.input_schema.clone())
    }

    fn response_schema(&self) -> Option<Value> {
        self.output_schema.clone()
    }

    fn annotations(&self) -> Option<&Value> {
        self.annotations.as_ref()
    }

    fn origin(&self) -> &str {
        &self.remote.origin
    }

    fn is_read_only(&self) -> bool {
        self.effect() == ToolEffect::ReadOnly
    }

    fn effect(&self) -> ToolEffect {
        if self.hint("readOnlyHint") == Some(true) {
            ToolEffect::ReadOnly
        } else if self.hint("idempotentHint") == Some(true) {
            ToolEffect::Idempotent
        } else {
            ToolEffect::NonIdempotent
        }
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let arguments = match args {
            Value::Null => Value::Object(Map::new()),
            arguments @ Value::Object(_) => arguments,
            _ => {
                return Err(AdkError::tool(format!(
                    "MCP tool '{}' takes an object of arguments",
                    self.name
                )));
            }
        };
        let mut params = Map::new();
        params.insert("name".into(), Value::String(self.name.clone()));
        params.insert("arguments".into(), arguments);
        let result = self.remote.request("tools/call", params).await?;
        if let Some(result_type) = result.get("resultType").and_then(Value::as_str)
            && result_type != "complete"
        {
            return Err(AdkError::tool(format!(
                "MCP tool '{}' answered with result type '{result_type}', which this agent does \
                 not support over ACP",
                self.name
            )));
        }
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            debug!(tool = self.name, "ACP client's MCP server answered the call with isError");
            return Ok(server_error_response(&self.name, &result));
        }
        call_result_to_value(&result).map_err(|error| {
            AdkError::tool(format!("MCP tool '{}' result invalid: {error}", self.name))
        })
    }
}

/// The text blocks of an MCP `content` array.
fn text_blocks(result: &Value) -> Vec<&str> {
    result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect()
}

/// The function response for a call the server answered with `isError: true`:
/// the tool's own error text, passed to the model as `{"error": text}` so it
/// can correct the call, cut to [`MAX_SERVER_ERROR_BYTES`].
fn server_error_response(tool_name: &str, result: &Value) -> Value {
    let text = text_blocks(result);
    let mut detail = if !text.is_empty() {
        text.join("\n")
    } else if let Some(structured) = result.get("structuredContent") {
        structured.to_string()
    } else {
        format!("MCP tool '{tool_name}' reported an error without a message")
    };
    if detail.len() > MAX_SERVER_ERROR_BYTES {
        detail.truncate(detail.floor_char_boundary(MAX_SERVER_ERROR_BYTES));
        detail.push_str(" [truncated]");
    }
    json!({ "error": detail })
}

/// Map an MCP `tools/call` result to ADK-Rust's tool-result envelope: text
/// and structured content as `output`, media as `inline_data`, and resource
/// links as `file_data`.
fn call_result_to_value(result: &Value) -> std::result::Result<Value, String> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut inline_data = Vec::new();
    let mut file_data = Vec::new();
    let decode = |data: Option<&Value>, what: &str| -> std::result::Result<Vec<u8>, String> {
        let data =
            data.and_then(Value::as_str).ok_or_else(|| format!("MCP {what} without data"))?;
        STANDARD.decode(data).map_err(|error| format!("invalid MCP {what} base64: {error}"))
    };
    let mime = |block: &Value, key: &str, default: &str| -> String {
        block.get(key).and_then(Value::as_str).unwrap_or(default).to_string()
    };

    for block in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                }
            }
            Some(kind @ ("image" | "audio")) => {
                let data = decode(block.get("data"), kind)?;
                inline_data.push(json!({
                    "mime_type": mime(block, "mimeType", "application/octet-stream"),
                    "data": data,
                }));
            }
            Some("resource") => {
                let resource = block.get("resource").cloned().unwrap_or(Value::Null);
                let uri = resource.get("uri").cloned().unwrap_or(Value::Null);
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                    file_data.push(json!({
                        "mime_type": mime(&resource, "mimeType", "text/plain"),
                        "file_uri": uri,
                    }));
                } else if resource.get("blob").is_some() {
                    let data = decode(resource.get("blob"), "resource")?;
                    let mime_type = mime(&resource, "mimeType", "application/octet-stream");
                    inline_data.push(json!({ "mime_type": mime_type, "data": data }));
                    file_data.push(json!({ "mime_type": mime_type, "file_uri": uri }));
                } else {
                    return Err("unsupported MCP embedded resource content".to_string());
                }
            }
            Some("resource_link") => file_data.push(json!({
                "mime_type": mime(block, "mimeType", "application/octet-stream"),
                "file_uri": block.get("uri").cloned().unwrap_or(Value::Null),
            })),
            _ => {}
        }
    }

    let output = match (result.get("structuredContent"), text_parts.is_empty()) {
        (Some(structured), true) => json!({ "output": structured }),
        (Some(structured), false) => json!({ "output": structured, "text": text_parts }),
        (None, false) => json!({ "output": text_parts.join("\n") }),
        (None, true) if !inline_data.is_empty() || !file_data.is_empty() => Value::Null,
        // A successful call may return nothing; reporting it as a failure would
        // invite a retry of a tool that already ran.
        (None, true) => json!({ "output": "" }),
    };
    if inline_data.is_empty() && file_data.is_empty() {
        Ok(output)
    } else {
        Ok(json!({ "response": output, "inline_data": inline_data, "file_data": file_data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_structured_results_map_like_stdio_mcp_tools() {
        let text = json!({ "content": [{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }] });
        assert_eq!(call_result_to_value(&text).unwrap(), json!({ "output": "a\nb" }));

        let structured = json!({
            "content": [{ "type": "text", "text": "{\"n\":1}" }],
            "structuredContent": { "n": 1 },
        });
        assert_eq!(
            call_result_to_value(&structured).unwrap(),
            json!({ "output": { "n": 1 }, "text": ["{\"n\":1}"] })
        );

        assert_eq!(
            call_result_to_value(&json!({ "content": [] })).unwrap(),
            json!({ "output": "" })
        );
    }

    #[test]
    fn media_results_use_the_inline_data_envelope() {
        let image =
            json!({ "content": [{ "type": "image", "data": "AAEC", "mimeType": "image/png" }] });
        let value = call_result_to_value(&image).unwrap();
        assert_eq!(value["inline_data"][0]["mime_type"], "image/png");
        assert_eq!(value["inline_data"][0]["data"], json!([0, 1, 2]));
        assert!(
            call_result_to_value(&json!({ "content": [{ "type": "image", "data": "%%" }] }))
                .is_err()
        );
    }

    #[test]
    fn is_error_results_keep_their_text_for_the_model() {
        let failed =
            json!({ "content": [{ "type": "text", "text": "no such order" }], "isError": true });
        assert_eq!(server_error_response("lookup", &failed), json!({ "error": "no such order" }));
        let long =
            json!({ "content": [{ "type": "text", "text": "é".repeat(MAX_SERVER_ERROR_BYTES) }] });
        let detail = server_error_response("lookup", &long)["error"].as_str().unwrap().to_string();
        assert!(detail.ends_with(" [truncated]"));
    }
}
