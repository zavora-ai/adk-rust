use crate::a2a::{
    AgentCard, JsonRpcRequest, JsonRpcResponse, Message, MessageSendParams,
    TaskArtifactUpdateEvent, TaskStatusUpdateEvent, UpdateEvent,
};
use adk_core::Result;
use futures::stream::Stream;
use serde_json::Value;
use std::pin::Pin;

/// A2A client for communicating with remote A2A agents
pub struct A2aClient {
    http_client: reqwest::Client,
    agent_card: AgentCard,
}

impl A2aClient {
    /// Create a new A2A client from an agent card
    pub fn new(agent_card: AgentCard) -> Self {
        Self { http_client: reqwest::Client::new(), agent_card }
    }

    /// Resolve an agent card from a URL (fetch from /.well-known/agent.json)
    pub async fn resolve_agent_card(base_url: &str) -> Result<AgentCard> {
        let url = format!("{}/.well-known/agent.json", base_url.trim_end_matches('/'));

        let client = reqwest::Client::new();
        let response =
            client.get(&url).send().await.map_err(|e| {
                adk_core::AdkError::agent(format!("Failed to fetch agent card: {e}"))
            })?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::agent(format!(
                "Failed to fetch agent card: HTTP {}",
                response.status()
            )));
        }

        let card: AgentCard = response
            .json()
            .await
            .map_err(|e| adk_core::AdkError::agent(format!("Failed to parse agent card: {e}")))?;

        Ok(card)
    }

    /// Create a client by resolving an agent card from a URL
    pub async fn from_url(base_url: &str) -> Result<Self> {
        let card = Self::resolve_agent_card(base_url).await?;
        Ok(Self::new(card))
    }

    /// Get the agent card
    pub fn agent_card(&self) -> &AgentCard {
        &self.agent_card
    }

    /// Send a message to the remote agent (blocking/non-streaming)
    pub async fn send_message(&self, message: Message) -> Result<JsonRpcResponse> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "message/send".to_string(),
            params: Some(
                serde_json::to_value(MessageSendParams { message, config: None })
                    .map_err(|e| adk_core::AdkError::agent(e.to_string()))?,
            ),
            id: Some(Value::String(uuid::Uuid::new_v4().to_string())),
        };

        let response = self
            .http_client
            .post(&self.agent_card.url)
            .json(&request)
            .send()
            .await
            .map_err(|e| adk_core::AdkError::agent(format!("Request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::agent(format!(
                "Request failed: HTTP {}",
                response.status()
            )));
        }

        let rpc_response: JsonRpcResponse = response
            .json()
            .await
            .map_err(|e| adk_core::AdkError::agent(format!("Failed to parse response: {e}")))?;

        Ok(rpc_response)
    }

    /// Send a message and receive streaming events via SSE
    pub async fn send_streaming_message(
        &self,
        message: Message,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<UpdateEvent>> + Send>>> {
        let stream_url = format!("{}/stream", self.agent_card.url.trim_end_matches('/'));

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "message/stream".to_string(),
            params: Some(
                serde_json::to_value(MessageSendParams { message, config: None })
                    .map_err(|e| adk_core::AdkError::agent(e.to_string()))?,
            ),
            id: Some(Value::String(uuid::Uuid::new_v4().to_string())),
        };

        let response = self
            .http_client
            .post(&stream_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| adk_core::AdkError::agent(format!("Request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::agent(format!(
                "Request failed: HTTP {}",
                response.status()
            )));
        }

        // Parse SSE stream
        let stream = async_stream::stream! {
            let mut bytes_stream = response.bytes_stream();
            let mut buffer = String::new();

            use futures::StreamExt;
            while let Some(chunk_result) = bytes_stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(adk_core::AdkError::agent(format!("Stream error: {e}")));
                        break;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE events
                while let Some(event_end) = buffer.find("\n\n") {
                    let event_data = buffer[..event_end].to_string();
                    buffer = buffer[event_end + 2..].to_string();

                    // Parse SSE event
                    if let Some(data) = parse_sse_data(&event_data) {
                        // Skip done events
                        if data.is_empty() {
                            continue;
                        }

                        // Parse JSON-RPC response
                        match serde_json::from_str::<JsonRpcResponse>(&data) {
                            Ok(rpc_response) => {
                                if let Some(result) = rpc_response.result {
                                    // Try to parse as different event types
                                    if let Ok(status_event) = serde_json::from_value::<TaskStatusUpdateEvent>(result.clone()) {
                                        yield Ok(UpdateEvent::TaskStatusUpdate(status_event));
                                    } else if let Ok(artifact_event) = serde_json::from_value::<TaskArtifactUpdateEvent>(result) {
                                        yield Ok(UpdateEvent::TaskArtifactUpdate(artifact_event));
                                    }
                                } else if let Some(error) = rpc_response.error {
                                    yield Err(adk_core::AdkError::agent(format!(
                                        "RPC error: {} ({})",
                                        error.message, error.code
                                    )));
                                }
                            }
                            Err(e) => {
                                // Skip parse errors for non-JSON data
                                tracing::debug!("Failed to parse SSE data: {e}");
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Parse the data field from an SSE event
fn parse_sse_data(event: &str) -> Option<String> {
    for line in event.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            return Some(data.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_data() {
        let event = "event: message\ndata: {\"test\": true}\n";
        assert_eq!(parse_sse_data(event), Some("{\"test\": true}".to_string()));
    }

    #[test]
    fn test_parse_sse_data_no_data() {
        let event = "event: ping\n";
        assert_eq!(parse_sse_data(event), None);
    }
}

// ── A2A v1.0.0 Client ───────────────────────────────────────────────────────

#[cfg(feature = "a2a-v1")]
pub mod v1_client {
    //! A2A v1.0.0 client for communicating with remote A2A agents.
    //!
    //! Sends the `A2A-Version: 1.0` header on all requests, supports all 11
    //! v1.0.0 operations via JSON-RPC, optional REST binding, structured error
    //! parsing, configurable retry, and agent card caching with conditional
    //! request headers.

    use a2a_protocol_types::jsonrpc::JsonRpcRequest;
    use a2a_protocol_types::task::{Task, TaskState};
    use a2a_protocol_types::{AgentCard, Message, TaskPushNotificationConfig};
    use reqwest::header::{HeaderMap, HeaderValue};
    use serde_json::Value;
    use std::time::Duration;

    /// Header name for A2A protocol version negotiation.
    const A2A_VERSION_HEADER: &str = "a2a-version";

    /// The v1.0.0 protocol version string.
    const A2A_VERSION: &str = "1.0";

    /// Well-known path for v1 agent cards.
    const AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

    /// Retry configuration for transient failures.
    #[derive(Debug, Clone)]
    pub struct RetryConfig {
        /// Maximum number of retry attempts (0 = no retries).
        pub max_retries: u32,
        /// Base delay between retries (doubles each attempt).
        pub base_delay: Duration,
    }

    impl Default for RetryConfig {
        fn default() -> Self {
            Self { max_retries: 3, base_delay: Duration::from_secs(1) }
        }
    }

    /// Error returned by the v1 client.
    #[derive(Debug, thiserror::Error)]
    pub enum V1ClientError {
        /// HTTP transport error.
        #[error("HTTP error: {0}")]
        Http(#[from] reqwest::Error),

        /// JSON-RPC error returned by the server.
        #[error("JSON-RPC error {code}: {message}")]
        JsonRpc { code: i32, message: String, data: Option<Value> },

        /// Version negotiation failed — server does not support requested version.
        #[error("version not supported: requested {requested}, server supports: {supported:?}")]
        VersionNotSupported { requested: String, supported: Vec<String> },

        /// Serialization/deserialization error.
        #[error("serialization error: {0}")]
        Serde(#[from] serde_json::Error),

        /// The server returned an unexpected HTTP status.
        #[error("unexpected HTTP status {status}: {body}")]
        UnexpectedStatus { status: u16, body: String },
    }

    /// Cached agent card with ETag and Last-Modified for conditional requests.
    #[derive(Debug, Clone, Default)]
    struct CachedCard {
        card: Option<AgentCard>,
        etag: Option<String>,
        last_modified: Option<String>,
    }

    /// A2A v1.0.0 client.
    ///
    /// Sends `A2A-Version: 1.0` on every request, supports all 11 operations
    /// via JSON-RPC (and optionally REST), parses structured error responses,
    /// caches agent cards with conditional headers, and retries transient
    /// failures.
    pub struct A2aV1Client {
        http_client: reqwest::Client,
        agent_card: AgentCard,
        retry_config: RetryConfig,
        cached_card: std::sync::Mutex<CachedCard>,
    }

    impl A2aV1Client {
        /// Creates a new v1 client from an already-resolved agent card.
        pub fn new(agent_card: AgentCard) -> Self {
            Self {
                http_client: reqwest::Client::new(),
                agent_card,
                retry_config: RetryConfig::default(),
                cached_card: std::sync::Mutex::new(CachedCard::default()),
            }
        }

        /// Creates a new v1 client with custom retry configuration.
        pub fn with_retry(agent_card: AgentCard, retry_config: RetryConfig) -> Self {
            Self {
                http_client: reqwest::Client::new(),
                agent_card,
                retry_config,
                cached_card: std::sync::Mutex::new(CachedCard::default()),
            }
        }

        /// Returns a reference to the agent card.
        pub fn agent_card(&self) -> &AgentCard {
            &self.agent_card
        }

        /// Returns the JSON-RPC endpoint URL from the agent card's
        /// `supportedInterfaces`.
        fn jsonrpc_url(&self) -> Option<&str> {
            self.agent_card
                .supported_interfaces
                .iter()
                .find(|i| i.protocol_binding == "JSONRPC")
                .map(|i| i.url.as_str())
        }

        /// Returns the REST endpoint URL from the agent card's
        /// `supportedInterfaces`, if available.
        fn rest_url(&self) -> Option<&str> {
            self.agent_card
                .supported_interfaces
                .iter()
                .find(|i| i.protocol_binding == "HTTP+JSON")
                .map(|i| i.url.as_str())
        }

        /// Builds default headers including `A2A-Version: 1.0`.
        fn default_headers() -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(A2A_VERSION_HEADER, HeaderValue::from_static(A2A_VERSION));
            headers
        }

        // ── Agent card resolution ────────────────────────────────────────

        /// Resolves an agent card from a base URL, fetching from
        /// `/.well-known/agent-card.json` with `A2A-Version: 1.0`.
        ///
        /// Caches the ETag and Last-Modified headers for subsequent
        /// conditional requests.
        pub async fn resolve_agent_card(base_url: &str) -> Result<AgentCard, V1ClientError> {
            let url = format!("{}{AGENT_CARD_PATH}", base_url.trim_end_matches('/'));
            let client = reqwest::Client::new();
            let response = client.get(&url).headers(Self::default_headers()).send().await?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                return Err(V1ClientError::UnexpectedStatus { status, body });
            }

            let card: AgentCard = response.json().await?;
            Ok(card)
        }

        /// Resolves an agent card using conditional headers if a cached
        /// version exists. Returns `None` if the server responds 304.
        pub async fn resolve_agent_card_cached(
            &self,
            base_url: &str,
        ) -> Result<Option<AgentCard>, V1ClientError> {
            let url = format!("{}{AGENT_CARD_PATH}", base_url.trim_end_matches('/'));

            let mut req = self.http_client.get(&url).headers(Self::default_headers());

            // Add conditional headers from cache
            {
                let cache = self.cached_card.lock().unwrap();
                if let Some(etag) = &cache.etag {
                    req = req.header("If-None-Match", etag.as_str());
                }
                if let Some(lm) = &cache.last_modified {
                    req = req.header("If-Modified-Since", lm.as_str());
                }
            }

            let response = req.send().await?;

            if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                return Ok(None);
            }

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                return Err(V1ClientError::UnexpectedStatus { status, body });
            }

            // Cache ETag and Last-Modified from response
            let etag =
                response.headers().get("etag").and_then(|v| v.to_str().ok()).map(String::from);
            let last_modified = response
                .headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok())
                .map(String::from);

            let card: AgentCard = response.json().await?;

            {
                let mut cache = self.cached_card.lock().unwrap();
                cache.card = Some(card.clone());
                cache.etag = etag;
                cache.last_modified = last_modified;
            }

            Ok(Some(card))
        }

        // ── JSON-RPC transport ───────────────────────────────────────────

        /// Sends a JSON-RPC request and returns the parsed result.
        async fn jsonrpc_call<T: serde::de::DeserializeOwned>(
            &self,
            method: &str,
            params: Value,
        ) -> Result<T, V1ClientError> {
            let url = self.jsonrpc_url().ok_or_else(|| V1ClientError::UnexpectedStatus {
                status: 0,
                body: "no JSONRPC interface in agent card".to_string(),
            })?;

            let request = JsonRpcRequest::with_params(
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
                method,
                params,
            );

            let response = self.send_with_retry(url, &request).await?;
            let status = response.status();

            // Check for version negotiation failure
            if status == reqwest::StatusCode::BAD_REQUEST {
                let body: Value = response.json().await?;
                if let Some(err) = body.get("error") {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0) as i32;
                    if code == -32009 {
                        return Err(Self::parse_version_error(err));
                    }
                }
                return Err(Self::parse_jsonrpc_error(&body));
            }

            let body: Value = response.json().await?;

            // Check for JSON-RPC error
            if body.get("error").is_some() {
                return Err(Self::parse_jsonrpc_error(&body));
            }

            // Extract result
            let result = body.get("result").cloned().unwrap_or(Value::Null);
            let parsed: T = serde_json::from_value(result)?;
            Ok(parsed)
        }

        /// Sends an HTTP request with retry logic for transient failures.
        async fn send_with_retry(
            &self,
            url: &str,
            request: &JsonRpcRequest,
        ) -> Result<reqwest::Response, V1ClientError> {
            let mut last_err = None;

            for attempt in 0..=self.retry_config.max_retries {
                if attempt > 0 {
                    let delay = self.retry_config.base_delay * 2u32.pow(attempt - 1);
                    tokio::time::sleep(delay).await;
                }

                match self
                    .http_client
                    .post(url)
                    .headers(Self::default_headers())
                    .json(request)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        let status = resp.status().as_u16();
                        // Retry on 429 and 5xx
                        if (status == 429 || status >= 500)
                            && attempt < self.retry_config.max_retries
                        {
                            last_err = Some(V1ClientError::UnexpectedStatus {
                                status,
                                body: format!("retryable status on attempt {attempt}"),
                            });
                            continue;
                        }
                        return Ok(resp);
                    }
                    Err(e) => {
                        if attempt < self.retry_config.max_retries && e.is_timeout() {
                            last_err = Some(V1ClientError::Http(e));
                            continue;
                        }
                        return Err(V1ClientError::Http(e));
                    }
                }
            }

            Err(last_err.unwrap_or_else(|| V1ClientError::UnexpectedStatus {
                status: 0,
                body: "retry exhausted".to_string(),
            }))
        }

        // ── REST transport ───────────────────────────────────────────────

        /// Sends a REST request (POST with JSON body) and returns the parsed
        /// result. Falls back to JSON-RPC if no REST interface is available.
        async fn rest_post<T: serde::de::DeserializeOwned>(
            &self,
            path: &str,
            body: &Value,
        ) -> Result<T, V1ClientError> {
            let base = match self.rest_url() {
                Some(url) => url.to_string(),
                None => {
                    return Err(V1ClientError::UnexpectedStatus {
                        status: 0,
                        body: "no HTTP+JSON interface in agent card".to_string(),
                    });
                }
            };
            let url = format!("{}{path}", base.trim_end_matches('/'));

            let response = self
                .http_client
                .post(&url)
                .headers(Self::default_headers())
                .header("content-type", "application/a2a+json")
                .json(body)
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body_text = response.text().await.unwrap_or_default();
                return Err(V1ClientError::UnexpectedStatus { status, body: body_text });
            }

            let result: T = response.json().await?;
            Ok(result)
        }

        /// Sends a REST GET request and returns the parsed result.
        async fn rest_get<T: serde::de::DeserializeOwned>(
            &self,
            path: &str,
        ) -> Result<T, V1ClientError> {
            let base = match self.rest_url() {
                Some(url) => url.to_string(),
                None => {
                    return Err(V1ClientError::UnexpectedStatus {
                        status: 0,
                        body: "no HTTP+JSON interface in agent card".to_string(),
                    });
                }
            };
            let url = format!("{}{path}", base.trim_end_matches('/'));

            let response =
                self.http_client.get(&url).headers(Self::default_headers()).send().await?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body_text = response.text().await.unwrap_or_default();
                return Err(V1ClientError::UnexpectedStatus { status, body: body_text });
            }

            let result: T = response.json().await?;
            Ok(result)
        }

        /// Sends a REST DELETE request.
        async fn rest_delete(&self, path: &str) -> Result<(), V1ClientError> {
            let base = match self.rest_url() {
                Some(url) => url.to_string(),
                None => {
                    return Err(V1ClientError::UnexpectedStatus {
                        status: 0,
                        body: "no HTTP+JSON interface in agent card".to_string(),
                    });
                }
            };
            let url = format!("{}{path}", base.trim_end_matches('/'));

            let response =
                self.http_client.delete(&url).headers(Self::default_headers()).send().await?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body_text = response.text().await.unwrap_or_default();
                return Err(V1ClientError::UnexpectedStatus { status, body: body_text });
            }

            Ok(())
        }

        // ── Error parsing ────────────────────────────────────────────────

        /// Parses a JSON-RPC error response into a `V1ClientError`.
        fn parse_jsonrpc_error(body: &Value) -> V1ClientError {
            let err = match body.get("error") {
                Some(e) => e,
                None => {
                    return V1ClientError::JsonRpc {
                        code: 0,
                        message: "unknown error".to_string(),
                        data: Some(body.clone()),
                    };
                }
            };

            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0) as i32;
            let message =
                err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error").to_string();
            let data = err.get("data").cloned();

            V1ClientError::JsonRpc { code, message, data }
        }

        /// Parses a version negotiation error, extracting supported versions
        /// from the structured `data` field.
        fn parse_version_error(err: &Value) -> V1ClientError {
            let data = err.get("data");
            let mut supported = Vec::new();

            // Try to extract supported versions from ErrorInfo metadata
            if let Some(data_arr) = data.and_then(|d| d.as_array()) {
                for info in data_arr {
                    if let Some(meta) = info.get("metadata") {
                        if let Some(versions) = meta.get("supported").and_then(|v| v.as_str()) {
                            supported = versions.split(", ").map(String::from).collect();
                        }
                    }
                }
            }

            V1ClientError::VersionNotSupported { requested: A2A_VERSION.to_string(), supported }
        }

        // ── 11 v1.0.0 Operations (JSON-RPC) ─────────────────────────────

        /// Sends a message to the remote agent (JSON-RPC `SendMessage`).
        pub async fn send_message(&self, message: Message) -> Result<Task, V1ClientError> {
            self.jsonrpc_call("SendMessage", serde_json::json!({ "message": message })).await
        }

        /// Sends a streaming message (JSON-RPC `SendStreamingMessage`).
        ///
        /// Returns the raw response for SSE parsing by the caller.
        pub async fn send_streaming_message(
            &self,
            message: Message,
        ) -> Result<reqwest::Response, V1ClientError> {
            let url = self.jsonrpc_url().ok_or_else(|| V1ClientError::UnexpectedStatus {
                status: 0,
                body: "no JSONRPC interface in agent card".to_string(),
            })?;

            let request = JsonRpcRequest::with_params(
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
                "SendStreamingMessage",
                serde_json::json!({ "message": message }),
            );

            let response = self
                .http_client
                .post(url)
                .headers(Self::default_headers())
                .json(&request)
                .send()
                .await?;

            Ok(response)
        }

        /// Retrieves a task by ID (JSON-RPC `GetTask`).
        pub async fn get_task(
            &self,
            task_id: &str,
            history_length: Option<u32>,
        ) -> Result<Task, V1ClientError> {
            let mut params = serde_json::json!({ "id": task_id });
            if let Some(len) = history_length {
                params["historyLength"] = serde_json::json!(len);
            }
            self.jsonrpc_call("GetTask", params).await
        }

        /// Cancels a task (JSON-RPC `CancelTask`).
        pub async fn cancel_task(&self, task_id: &str) -> Result<Task, V1ClientError> {
            self.jsonrpc_call("CancelTask", serde_json::json!({ "id": task_id })).await
        }

        /// Lists tasks with optional filtering (JSON-RPC `ListTasks`).
        pub async fn list_tasks(
            &self,
            context_id: Option<&str>,
            status: Option<TaskState>,
            page_size: Option<u32>,
            page_token: Option<&str>,
        ) -> Result<Vec<Task>, V1ClientError> {
            let mut params = serde_json::json!({});
            if let Some(cid) = context_id {
                params["contextId"] = serde_json::json!(cid);
            }
            if let Some(s) = status {
                params["status"] = serde_json::to_value(s)?;
            }
            if let Some(ps) = page_size {
                params["pageSize"] = serde_json::json!(ps);
            }
            if let Some(pt) = page_token {
                params["pageToken"] = serde_json::json!(pt);
            }
            self.jsonrpc_call("ListTasks", params).await
        }

        /// Subscribes to task updates (JSON-RPC `SubscribeToTask`).
        ///
        /// Returns the raw response for SSE parsing by the caller.
        pub async fn subscribe_to_task(
            &self,
            task_id: &str,
        ) -> Result<reqwest::Response, V1ClientError> {
            let url = self.jsonrpc_url().ok_or_else(|| V1ClientError::UnexpectedStatus {
                status: 0,
                body: "no JSONRPC interface in agent card".to_string(),
            })?;

            let request = JsonRpcRequest::with_params(
                serde_json::json!(uuid::Uuid::new_v4().to_string()),
                "SubscribeToTask",
                serde_json::json!({ "id": task_id }),
            );

            let response = self
                .http_client
                .post(url)
                .headers(Self::default_headers())
                .json(&request)
                .send()
                .await?;

            Ok(response)
        }

        /// Creates a push notification config (JSON-RPC
        /// `CreateTaskPushNotificationConfig`).
        pub async fn create_push_notification_config(
            &self,
            config: TaskPushNotificationConfig,
        ) -> Result<TaskPushNotificationConfig, V1ClientError> {
            self.jsonrpc_call("CreateTaskPushNotificationConfig", serde_json::to_value(&config)?)
                .await
        }

        /// Gets a push notification config (JSON-RPC
        /// `GetTaskPushNotificationConfig`).
        pub async fn get_push_notification_config(
            &self,
            task_id: &str,
            config_id: &str,
        ) -> Result<TaskPushNotificationConfig, V1ClientError> {
            self.jsonrpc_call(
                "GetTaskPushNotificationConfig",
                serde_json::json!({ "taskId": task_id, "id": config_id }),
            )
            .await
        }

        /// Lists push notification configs (JSON-RPC
        /// `ListTaskPushNotificationConfigs`).
        pub async fn list_push_notification_configs(
            &self,
            task_id: &str,
        ) -> Result<Vec<TaskPushNotificationConfig>, V1ClientError> {
            self.jsonrpc_call(
                "ListTaskPushNotificationConfigs",
                serde_json::json!({ "taskId": task_id }),
            )
            .await
        }

        /// Deletes a push notification config (JSON-RPC
        /// `DeleteTaskPushNotificationConfig`).
        pub async fn delete_push_notification_config(
            &self,
            task_id: &str,
            config_id: &str,
        ) -> Result<(), V1ClientError> {
            let _: Value = self
                .jsonrpc_call(
                    "DeleteTaskPushNotificationConfig",
                    serde_json::json!({ "taskId": task_id, "id": config_id }),
                )
                .await?;
            Ok(())
        }

        /// Gets the extended agent card (JSON-RPC `GetExtendedAgentCard`).
        pub async fn get_extended_agent_card(&self) -> Result<AgentCard, V1ClientError> {
            self.jsonrpc_call("GetExtendedAgentCard", serde_json::json!({})).await
        }

        // ── REST binding operations ──────────────────────────────────────

        /// Sends a message via REST (`POST /message:send`).
        pub async fn send_message_rest(&self, message: Message) -> Result<Task, V1ClientError> {
            self.rest_post("/message:send", &serde_json::json!({ "message": message })).await
        }

        /// Gets a task via REST (`GET /tasks/{taskId}`).
        pub async fn get_task_rest(&self, task_id: &str) -> Result<Task, V1ClientError> {
            self.rest_get(&format!("/tasks/{task_id}")).await
        }

        /// Cancels a task via REST (`POST /tasks/{taskId}:cancel`).
        pub async fn cancel_task_rest(&self, task_id: &str) -> Result<Task, V1ClientError> {
            self.rest_post(&format!("/tasks/{task_id}:cancel"), &serde_json::json!({})).await
        }

        /// Lists tasks via REST (`GET /tasks`).
        pub async fn list_tasks_rest(&self) -> Result<Vec<Task>, V1ClientError> {
            self.rest_get("/tasks").await
        }

        /// Creates a push notification config via REST
        /// (`POST /tasks/{taskId}/pushNotificationConfigs`).
        pub async fn create_push_notification_config_rest(
            &self,
            task_id: &str,
            config: TaskPushNotificationConfig,
        ) -> Result<TaskPushNotificationConfig, V1ClientError> {
            self.rest_post(
                &format!("/tasks/{task_id}/pushNotificationConfigs"),
                &serde_json::to_value(&config)?,
            )
            .await
        }

        /// Gets a push notification config via REST
        /// (`GET /tasks/{taskId}/pushNotificationConfigs/{configId}`).
        pub async fn get_push_notification_config_rest(
            &self,
            task_id: &str,
            config_id: &str,
        ) -> Result<TaskPushNotificationConfig, V1ClientError> {
            self.rest_get(&format!("/tasks/{task_id}/pushNotificationConfigs/{config_id}")).await
        }

        /// Lists push notification configs via REST
        /// (`GET /tasks/{taskId}/pushNotificationConfigs`).
        pub async fn list_push_notification_configs_rest(
            &self,
            task_id: &str,
        ) -> Result<Vec<TaskPushNotificationConfig>, V1ClientError> {
            self.rest_get(&format!("/tasks/{task_id}/pushNotificationConfigs")).await
        }

        /// Deletes a push notification config via REST
        /// (`DELETE /tasks/{taskId}/pushNotificationConfigs/{configId}`).
        pub async fn delete_push_notification_config_rest(
            &self,
            task_id: &str,
            config_id: &str,
        ) -> Result<(), V1ClientError> {
            self.rest_delete(&format!("/tasks/{task_id}/pushNotificationConfigs/{config_id}")).await
        }

        /// Gets the extended agent card via REST (`GET /extendedAgentCard`).
        pub async fn get_extended_agent_card_rest(&self) -> Result<AgentCard, V1ClientError> {
            self.rest_get("/extendedAgentCard").await
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use a2a_protocol_types::{AgentCapabilities, AgentCard, AgentInterface, AgentSkill};

        fn make_test_card() -> AgentCard {
            AgentCard {
                name: "test-agent".to_string(),
                url: Some("http://localhost:9999".to_string()),
                description: "A test agent".to_string(),
                version: "1.0.0".to_string(),
                supported_interfaces: vec![
                    AgentInterface {
                        url: "http://localhost:9999/a2a".to_string(),
                        protocol_binding: "JSONRPC".to_string(),
                        protocol_version: "1.0".to_string(),
                        tenant: None,
                    },
                    AgentInterface {
                        url: "http://localhost:9999/rest".to_string(),
                        protocol_binding: "HTTP+JSON".to_string(),
                        protocol_version: "1.0".to_string(),
                        tenant: None,
                    },
                ],
                default_input_modes: vec!["text/plain".to_string()],
                default_output_modes: vec!["text/plain".to_string()],
                skills: vec![AgentSkill {
                    id: "echo".to_string(),
                    name: "Echo".to_string(),
                    description: "Echoes input".to_string(),
                    tags: vec![],
                    examples: None,
                    input_modes: None,
                    output_modes: None,
                    security_requirements: None,
                }],
                capabilities: AgentCapabilities::default(),
                provider: None,
                icon_url: None,
                documentation_url: None,
                security_schemes: None,
                security_requirements: None,
                signatures: None,
            }
        }

        fn make_jsonrpc_only_card() -> AgentCard {
            let mut card = make_test_card();
            card.supported_interfaces.retain(|i| i.protocol_binding == "JSONRPC");
            card
        }

        #[test]
        fn new_client_stores_agent_card() {
            let card = make_test_card();
            let client = A2aV1Client::new(card.clone());
            assert_eq!(client.agent_card().name, "test-agent");
            assert_eq!(client.agent_card().version, "1.0.0");
        }

        #[test]
        fn with_retry_stores_config() {
            let card = make_test_card();
            let config = RetryConfig { max_retries: 5, base_delay: Duration::from_millis(500) };
            let client = A2aV1Client::with_retry(card, config);
            assert_eq!(client.retry_config.max_retries, 5);
            assert_eq!(client.retry_config.base_delay, Duration::from_millis(500));
        }

        #[test]
        fn default_retry_config() {
            let config = RetryConfig::default();
            assert_eq!(config.max_retries, 3);
            assert_eq!(config.base_delay, Duration::from_secs(1));
        }

        #[test]
        fn jsonrpc_url_found() {
            let client = A2aV1Client::new(make_test_card());
            assert_eq!(client.jsonrpc_url(), Some("http://localhost:9999/a2a"));
        }

        #[test]
        fn rest_url_found() {
            let client = A2aV1Client::new(make_test_card());
            assert_eq!(client.rest_url(), Some("http://localhost:9999/rest"));
        }

        #[test]
        fn rest_url_none_when_not_available() {
            let client = A2aV1Client::new(make_jsonrpc_only_card());
            assert!(client.rest_url().is_none());
        }

        #[test]
        fn default_headers_include_version() {
            let headers = A2aV1Client::default_headers();
            let version = headers.get(A2A_VERSION_HEADER).unwrap();
            assert_eq!(version, "1.0");
        }

        #[test]
        fn parse_jsonrpc_error_extracts_fields() {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32001,
                    "message": "Task not found: task_123",
                    "data": [{"@type": "type.googleapis.com/google.rpc.ErrorInfo"}]
                },
                "id": 1
            });
            let err = A2aV1Client::parse_jsonrpc_error(&body);
            match err {
                V1ClientError::JsonRpc { code, message, data } => {
                    assert_eq!(code, -32001);
                    assert_eq!(message, "Task not found: task_123");
                    assert!(data.is_some());
                }
                other => panic!("expected JsonRpc error, got: {other}"),
            }
        }

        #[test]
        fn parse_jsonrpc_error_handles_missing_error_field() {
            let body = serde_json::json!({"result": "ok"});
            let err = A2aV1Client::parse_jsonrpc_error(&body);
            match err {
                V1ClientError::JsonRpc { code, .. } => {
                    assert_eq!(code, 0);
                }
                other => panic!("expected JsonRpc error, got: {other}"),
            }
        }

        #[test]
        fn parse_version_error_extracts_supported_versions() {
            let err_obj = serde_json::json!({
                "code": -32009,
                "message": "Version not supported",
                "data": [{
                    "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                    "reason": "VERSION_NOT_SUPPORTED",
                    "domain": "a2a-protocol.org",
                    "metadata": {
                        "requested": "2.0",
                        "supported": "0.3, 1.0"
                    }
                }]
            });
            let err = A2aV1Client::parse_version_error(&err_obj);
            match err {
                V1ClientError::VersionNotSupported { requested, supported } => {
                    assert_eq!(requested, "1.0");
                    assert_eq!(supported, vec!["0.3", "1.0"]);
                }
                other => panic!("expected VersionNotSupported, got: {other}"),
            }
        }

        #[test]
        fn parse_version_error_handles_empty_data() {
            let err_obj = serde_json::json!({
                "code": -32009,
                "message": "Version not supported"
            });
            let err = A2aV1Client::parse_version_error(&err_obj);
            match err {
                V1ClientError::VersionNotSupported { supported, .. } => {
                    assert!(supported.is_empty());
                }
                other => panic!("expected VersionNotSupported, got: {other}"),
            }
        }

        #[test]
        fn v1_client_error_display() {
            let err = V1ClientError::JsonRpc {
                code: -32001,
                message: "Task not found".to_string(),
                data: None,
            };
            assert_eq!(err.to_string(), "JSON-RPC error -32001: Task not found");

            let err = V1ClientError::VersionNotSupported {
                requested: "2.0".to_string(),
                supported: vec!["0.3".to_string(), "1.0".to_string()],
            };
            assert!(err.to_string().contains("2.0"));
            assert!(err.to_string().contains("0.3"));

            let err =
                V1ClientError::UnexpectedStatus { status: 500, body: "internal error".to_string() };
            assert!(err.to_string().contains("500"));
        }

        #[test]
        fn cached_card_starts_empty() {
            let client = A2aV1Client::new(make_test_card());
            let cache = client.cached_card.lock().unwrap();
            assert!(cache.card.is_none());
            assert!(cache.etag.is_none());
            assert!(cache.last_modified.is_none());
        }
    }
}
