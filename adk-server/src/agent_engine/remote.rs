//! Remote ReasoningEngine invocation — call other deployed Agent Engine
//! agents as sub-agents.
//!
//! [`RemoteReasoningEngineAgent`] implements [`adk_core::Agent`] by
//! forwarding each turn to a deployed engine's public
//! `reasoningEngines:streamQuery` surface (`?alt=sse`) and converting the
//! streamed events back into ADK [`Event`]s. It follows the
//! [`RemoteA2aAgent`](crate::a2a::RemoteA2aAgent) precedent: a leaf agent
//! (`sub_agents()` is empty) whose stream carries remote results.
//!
//! The **public API envelope** is `{"classMethod": ..., "input": ...}`
//! (canonical camelCase per the REST reference; the platform maps it onto
//! the container's snake_case dispatch envelope). Responses arrive as SSE
//! `data:` frames whose payloads are the same event JSON the server-side
//! dispatcher emits — wire compatibility with the dispatch module is
//! enforced by a shared fixture round-trip test.
//!
//! Engines are addressed by full resource name, or by Agent Registry URN
//! when a registry client is supplied at build time (the engine resource
//! name is read from the registry entry's `RuntimeReference` attribute).

use adk_core::{Agent, Content, ErrorComponent, Event, EventStream, InvocationContext, Result};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient};
use async_trait::async_trait;
use reqwest::Method;
use serde_json::{Value, json};
use std::sync::Arc;

const API_VERSION: &str = "v1";
const DEFAULT_CLASS_METHOD: &str = "streaming_agent_run_with_events";
const DEFAULT_FALLBACK_CLASS_METHOD: &str = "stream_query";
const RUNTIME_REFERENCE_ATTRIBUTE: &str = "agentregistry.googleapis.com/system/RuntimeReference";

const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "server.remote_engine.invalid_input",
    unauthorized: "server.remote_engine.unauthorized",
    forbidden: "server.remote_engine.forbidden",
    not_found: "server.remote_engine.not_found",
    rate_limited: "server.remote_engine.rate_limited",
    timeout: "server.remote_engine.timeout",
    unavailable: "server.remote_engine.unavailable",
    credentials_unavailable: "server.remote_engine.credentials_unavailable",
    invalid_response: "server.remote_engine.invalid_response",
    invalid_request: "server.remote_engine.invalid_request",
    upstream_error: "server.remote_engine.upstream_error",
    operation_failed: "server.remote_engine.operation_failed",
};

fn gcp_error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Server, GCP_ERROR_CODES, "remote reasoning engine")
}

/// Builder for [`RemoteReasoningEngineAgent`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::agent_engine::remote::RemoteReasoningEngineAgent;
///
/// # async fn build() -> adk_core::Result<()> {
/// let agent = RemoteReasoningEngineAgent::builder("research-agent")
///     .resource_name("projects/my-project/locations/us-central1/reasoningEngines/4242")
///     .build()
///     .await?;
/// # let _ = agent;
/// # Ok(())
/// # }
/// ```
pub struct RemoteReasoningEngineAgentBuilder {
    name: String,
    description: String,
    resource_name: Option<String>,
    urn: Option<String>,
    registry: Option<adk_tool::AgentRegistryClient>,
    class_method: String,
    fallback_class_method: Option<String>,
    endpoint: Option<String>,
    credentials: Option<google_cloud_auth::credentials::Credentials>,
}

impl RemoteReasoningEngineAgentBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: "Remote Agent Engine agent".to_string(),
            resource_name: None,
            urn: None,
            registry: None,
            class_method: DEFAULT_CLASS_METHOD.to_string(),
            fallback_class_method: Some(DEFAULT_FALLBACK_CLASS_METHOD.to_string()),
            endpoint: None,
            credentials: None,
        }
    }

    /// Sets the agent description shown to orchestrating LLMs.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Addresses the engine by full resource name
    /// (`projects/*/locations/*/reasoningEngines/*`).
    #[must_use]
    pub fn resource_name(mut self, resource_name: impl Into<String>) -> Self {
        self.resource_name = Some(resource_name.into());
        self
    }

    /// Addresses the engine by Agent Registry URN, resolved at build time
    /// via [`registry`](Self::registry).
    #[must_use]
    pub fn urn(mut self, urn: impl Into<String>) -> Self {
        self.urn = Some(urn.into());
        self
    }

    /// Supplies the registry client used to resolve a [`urn`](Self::urn).
    #[must_use]
    pub fn registry(mut self, registry: adk_tool::AgentRegistryClient) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Overrides the class method invoked on the engine.
    ///
    /// Defaults to `streaming_agent_run_with_events` with a one-shot
    /// fallback to `stream_query` when the engine does not register it.
    #[must_use]
    pub fn class_method(mut self, class_method: impl Into<String>) -> Self {
        self.class_method = class_method.into();
        self.fallback_class_method = None;
        self
    }

    /// Sets a custom API origin (loopback HTTP allowed for tests).
    #[must_use]
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Uses explicit credentials instead of Application Default Credentials.
    #[must_use]
    pub fn credentials(mut self, credentials: google_cloud_auth::credentials::Credentials) -> Self {
        self.credentials = Some(credentials);
        self
    }

    /// Resolves the engine address and builds the agent.
    ///
    /// # Errors
    ///
    /// Returns an error when neither `resource_name` nor a resolvable
    /// `urn` + `registry` pair is configured, the resource name is not a
    /// valid `reasoningEngines` name, or the HTTP client cannot be built.
    pub async fn build(self) -> Result<RemoteReasoningEngineAgent> {
        let errors = gcp_error_context();
        let resource_name = match (self.resource_name, self.urn) {
            (Some(resource_name), _) => resource_name,
            (None, Some(urn)) => {
                let registry = self.registry.as_ref().ok_or_else(|| {
                    errors.invalid_input(
                        "a URN address requires a registry client; call registry(...) or use resource_name(...)",
                    )
                })?;
                resolve_urn(registry, &urn).await?
            }
            (None, None) => {
                return Err(errors.invalid_input(
                    "the remote engine address is required; call resource_name(...) or urn(...) + registry(...)",
                ));
            }
        };

        let location = location_of(&resource_name).ok_or_else(|| {
            errors.invalid_input(format!(
                "'{resource_name}' is not a projects/*/locations/*/reasoningEngines/* resource name",
            ))
        })?;
        let endpoint = self
            .endpoint
            .unwrap_or_else(|| format!("https://{location}-aiplatform.googleapis.com"));

        let mut builder =
            GcpHttpClient::builder(gcp_error_context(), endpoint).api_version(API_VERSION);
        if let Some(credentials) = self.credentials {
            builder = builder.credentials(credentials);
        }
        let client = builder.build()?;

        Ok(RemoteReasoningEngineAgent {
            name: self.name,
            description: self.description,
            resource_name,
            class_method: self.class_method,
            fallback_class_method: self.fallback_class_method,
            client: Arc::new(client),
        })
    }
}

/// An [`Agent`] that forwards each turn to a deployed Agent Engine.
pub struct RemoteReasoningEngineAgent {
    name: String,
    description: String,
    resource_name: String,
    class_method: String,
    fallback_class_method: Option<String>,
    client: Arc<GcpHttpClient>,
}

impl RemoteReasoningEngineAgent {
    /// Starts building a remote agent with the given local name.
    pub fn builder(name: impl Into<String>) -> RemoteReasoningEngineAgentBuilder {
        RemoteReasoningEngineAgentBuilder::new(name)
    }

    /// The engine resource name this agent invokes.
    pub fn resource_name(&self) -> &str {
        &self.resource_name
    }

    fn input_for(&self, class_method: &str, ctx: &dyn InvocationContext) -> Value {
        let message = user_text(ctx.user_content());
        if class_method == DEFAULT_CLASS_METHOD {
            // The dispatcher-side AgentRunRequest, carried as a JSON string
            // per the platform contract (shared wire fixture, WP1).
            let request = json!({
                "appName": ctx.app_name(),
                "userId": ctx.user_id(),
                "sessionId": ctx.session_id(),
                "newMessage": { "role": "user", "parts": [{ "text": message }] },
                "streaming": true,
            });
            json!({ "request_json": request.to_string() })
        } else {
            json!({
                "user_id": ctx.user_id(),
                "session_id": ctx.session_id(),
                "message": message,
            })
        }
    }

    async fn open_stream(
        &self,
        class_method: &str,
        ctx: &dyn InvocationContext,
    ) -> Result<reqwest::Response> {
        let errors = gcp_error_context();
        let body = json!({
            // Canonical camelCase on the public API (the container's
            // snake_case envelope does not survive the platform boundary).
            "classMethod": class_method,
            "input": self.input_for(class_method, ctx),
        });
        let request = self
            .client
            .request(Method::POST, &format!("{}:streamQuery", self.resource_name))
            .await?
            .query(&[("alt", "sse")])
            .json(&body);
        let response = request.send().await.map_err(|error| {
            errors.unavailable(format!(
                "failed to send remote reasoning engine request: {}",
                adk_gcp::truncate_for_error(&error.without_url().to_string()),
            ))
        })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(errors.status_error(status, &body));
        }
        Ok(response)
    }

    /// Whether a failed primary method should be retried with the fallback.
    fn should_fall_back(error: &adk_core::AdkError) -> bool {
        matches!(error.details.upstream_status_code, Some(400 | 404 | 422 | 501))
    }
}

#[async_trait]
impl Agent for RemoteReasoningEngineAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let agent = RemoteReasoningEngineAgent {
            name: self.name.clone(),
            description: self.description.clone(),
            resource_name: self.resource_name.clone(),
            class_method: self.class_method.clone(),
            fallback_class_method: self.fallback_class_method.clone(),
            client: Arc::clone(&self.client),
        };
        let invocation_id = ctx.invocation_id().to_string();
        let agent_name = self.name.clone();

        let stream = async_stream::stream! {
            let response = match agent.open_stream(&agent.class_method, ctx.as_ref()).await {
                Ok(response) => Ok(response),
                Err(error)
                    if RemoteReasoningEngineAgent::should_fall_back(&error)
                        && agent.fallback_class_method.is_some() =>
                {
                    let fallback = agent.fallback_class_method.as_deref().unwrap();
                    tracing::debug!(
                        remote.class_method = %agent.class_method,
                        remote.fallback = %fallback,
                        "primary class method rejected; retrying with fallback"
                    );
                    agent.open_stream(fallback, ctx.as_ref()).await
                }
                Err(error) => Err(error),
            };
            let mut response = match response {
                Ok(response) => response,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };

            let mut parser = SseFrameParser::default();
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(error) => {
                        yield Ok(stream_error_event(
                            &invocation_id,
                            &agent_name,
                            &format!(
                                "remote reasoning engine stream failed: {}",
                                adk_gcp::truncate_for_error(&error.without_url().to_string()),
                            ),
                        ));
                        return;
                    }
                };
                for payload in parser.push(&chunk) {
                    match parse_remote_event(&payload) {
                        Ok(event) => yield Ok(event),
                        Err(message) => {
                            yield Ok(stream_error_event(&invocation_id, &agent_name, &message));
                            return;
                        }
                    }
                }
            }
            for payload in parser.finish() {
                match parse_remote_event(&payload) {
                    Ok(event) => yield Ok(event),
                    Err(message) => {
                        yield Ok(stream_error_event(&invocation_id, &agent_name, &message));
                        return;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Incremental SSE parser that buffers across chunk boundaries.
///
/// Frames are separated by blank lines; each frame's `data:` lines are
/// joined per the SSE specification. Bare JSON lines without SSE framing
/// are tolerated (the platform has been observed emitting both shapes).
#[derive(Default)]
struct SseFrameParser {
    buffer: String,
}

impl SseFrameParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut payloads = Vec::new();
        while let Some(boundary) = self
            .buffer
            .find("\n\n")
            .map(|i| (i, 2))
            .or_else(|| self.buffer.find("\r\n\r\n").map(|i| (i, 4)))
        {
            let frame: String = self.buffer.drain(..boundary.0 + boundary.1).collect();
            if let Some(payload) = frame_payload(&frame) {
                payloads.push(payload);
            }
        }
        payloads
    }

    fn finish(&mut self) -> Vec<String> {
        let rest = std::mem::take(&mut self.buffer);
        frame_payload(&rest).into_iter().collect()
    }
}

/// Extracts the data payload of one SSE frame (or bare JSON lines).
fn frame_payload(frame: &str) -> Option<String> {
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        } else if !line.is_empty() && !line.starts_with(':') && !line.contains(':') {
            // Not SSE-framed and not a field line: tolerate bare payloads.
            data_lines.push(line);
        }
    }
    if data_lines.is_empty() {
        let trimmed = frame.trim();
        if trimmed.is_empty() {
            return None;
        }
        // A frame with no data: lines but non-field content (bare NDJSON).
        return Some(trimmed.to_string());
    }
    Some(data_lines.join("\n"))
}

/// Parses one streamed payload into an ADK [`Event`].
fn parse_remote_event(payload: &str) -> std::result::Result<Event, String> {
    let value: Value = serde_json::from_str(payload).map_err(|error| {
        format!(
            "failed to parse remote reasoning engine event JSON: {}",
            adk_gcp::truncate_for_error(&error.to_string()),
        )
    })?;
    if let Some(error) = value.get("error") {
        return Err(format!(
            "remote reasoning engine reported an error mid-stream: {}",
            adk_gcp::truncate_for_error(&error.to_string()),
        ));
    }
    serde_json::from_value(value).map_err(|error| {
        format!(
            "remote reasoning engine event does not match the ADK event shape: {}",
            adk_gcp::truncate_for_error(&error.to_string()),
        )
    })
}

fn stream_error_event(invocation_id: &str, agent_name: &str, message: &str) -> Event {
    let mut event = Event::new(invocation_id.to_string());
    event.author = agent_name.to_string();
    event.llm_response.error_message = Some(message.to_string());
    event.llm_response.turn_complete = true;
    event
}

/// Extracts the user's text from the invocation content.
fn user_text(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            adk_core::Part::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolves an Agent Registry URN to an engine resource name via the
/// entry's `RuntimeReference` attribute.
async fn resolve_urn(registry: &adk_tool::AgentRegistryClient, urn: &str) -> Result<String> {
    let errors = gcp_error_context();
    let agent = registry.get_agent(urn).await?;
    let uri = agent
        .attributes
        .as_ref()
        .and_then(|attributes| attributes.get(RUNTIME_REFERENCE_ATTRIBUTE))
        .and_then(|attribute| attribute.get("uri"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            errors.invalid_input(format!(
                "registry entry '{urn}' carries no RuntimeReference attribute; only Agent Engine deployments can be invoked as remote reasoning engines",
            ))
        })?;
    let resource_name = uri.trim_start_matches('/');
    let resource_name = resource_name
        .strip_prefix("aiplatform.googleapis.com/")
        .unwrap_or(resource_name)
        .to_string();
    if location_of(&resource_name).is_none() {
        return Err(errors.invalid_input(format!(
            "registry entry '{urn}' resolves to '{resource_name}', which is not a reasoningEngines resource name",
        )));
    }
    Ok(resource_name)
}

/// Extracts the location segment of a `reasoningEngines` resource name.
fn location_of(resource_name: &str) -> Option<&str> {
    let mut segments = resource_name.split('/');
    let (
        Some("projects"),
        Some(_),
        Some("locations"),
        Some(location),
        Some("reasoningEngines"),
        Some(id),
    ) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    )
    else {
        return None;
    };
    if location.is_empty() || id.is_empty() { None } else { Some(location) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_parse_only_from_engine_resource_names() {
        assert_eq!(
            location_of("projects/p/locations/us-central1/reasoningEngines/42"),
            Some("us-central1"),
        );
        assert_eq!(location_of("projects/p/locations/us-central1/reasoningEngines/"), None);
        assert_eq!(location_of("projects/p/locations/us-central1/agents/42"), None);
        assert_eq!(location_of("us-central1"), None);
    }

    #[test]
    fn sse_frames_survive_chunk_splits_and_bare_lines() {
        let mut parser = SseFrameParser::default();
        let mut payloads = parser.push(b"data: {\"a\":");
        assert!(payloads.is_empty(), "incomplete frame must buffer");
        payloads.extend(parser.push(b" 1}\n\ndata: {\"b\": 2}\n\n"));
        assert_eq!(payloads, vec!["{\"a\": 1}".to_string(), "{\"b\": 2}".to_string()]);

        let mut parser = SseFrameParser::default();
        let payloads = parser.push(b"{\"bare\": true}\n\n");
        assert_eq!(payloads, vec!["{\"bare\": true}".to_string()]);
    }

    #[test]
    fn multi_line_data_frames_join_per_sse_spec() {
        let mut parser = SseFrameParser::default();
        let payloads = parser.push(b"data: {\"a\":\ndata: 1}\n\n");
        assert_eq!(payloads, vec!["{\"a\":\n1}".to_string()]);
    }

    #[test]
    fn error_payloads_terminate_with_a_message() {
        let error = parse_remote_event("{\"error\": {\"code\": 404, \"message\": \"gone\"}}")
            .expect_err("error payloads must not parse as events");
        assert!(error.contains("mid-stream"), "{error}");
    }
}
