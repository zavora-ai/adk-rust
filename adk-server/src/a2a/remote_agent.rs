use crate::a2a::{A2aClient, Part as A2aPart, Role, UpdateEvent};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Part, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// Configuration for a remote A2A agent
#[derive(Clone)]
pub struct RemoteA2aConfig {
    /// Name of the agent
    pub name: String,
    /// Description of the agent
    pub description: String,
    /// Base URL of the remote agent (e.g., "http://localhost:8080")
    /// The agent card will be fetched from {base_url}/.well-known/agent.json
    pub agent_url: String,
}

/// An agent that communicates with a remote A2A agent
pub struct RemoteA2aAgent {
    config: RemoteA2aConfig,
}

impl RemoteA2aAgent {
    pub fn new(config: RemoteA2aConfig) -> Self {
        Self { config }
    }

    pub fn builder(name: impl Into<String>) -> RemoteA2aAgentBuilder {
        RemoteA2aAgentBuilder::new(name)
    }
}

#[async_trait]
impl Agent for RemoteA2aAgent {
    fn name(&self) -> &str {
        &self.config.name
    }

    fn description(&self) -> &str {
        &self.config.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let url = self.config.agent_url.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let agent_name = self.config.name.clone();

        // Get user content from context
        let user_content = get_user_content_from_context(ctx.as_ref());

        let stream = async_stream::stream! {
            // Create A2A client
            let client = match A2aClient::from_url(&url).await {
                Ok(c) => c,
                Err(e) => {
                    yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                    return;
                }
            };

            // Build message from user content
            let message = build_a2a_message(user_content);

            // Send streaming message
            match client.send_streaming_message(message).await {
                Ok(mut event_stream) => {
                    use futures::StreamExt;
                    while let Some(result) = event_stream.next().await {
                        match result {
                            Ok(update_event) => {
                                if let Some(event) = convert_update_event(&invocation_id, &agent_name, update_event) {
                                    yield Ok(event);
                                }
                            }
                            Err(e) => {
                                yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                                return;
                            }
                        }
                    }
                }
                Err(e) => {
                    yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Builder for RemoteA2aAgent
pub struct RemoteA2aAgentBuilder {
    name: String,
    description: String,
    agent_url: Option<String>,
}

impl RemoteA2aAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), description: String::new(), agent_url: None }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn agent_url(mut self, url: impl Into<String>) -> Self {
        self.agent_url = Some(url.into());
        self
    }

    pub fn build(self) -> Result<RemoteA2aAgent> {
        let agent_url = self
            .agent_url
            .ok_or_else(|| adk_core::AdkError::agent("RemoteA2aAgent requires agent_url"))?;

        Ok(RemoteA2aAgent::new(RemoteA2aConfig {
            name: self.name,
            description: self.description,
            agent_url,
        }))
    }
}

// Helper functions

fn get_user_content_from_context(ctx: &dyn InvocationContext) -> Option<String> {
    let content = ctx.user_content();
    for part in &content.parts {
        if let Part::Text { text } = part {
            return Some(text.clone());
        }
    }
    None
}

fn build_a2a_message(content: Option<String>) -> crate::a2a::Message {
    let text = content.unwrap_or_default();
    crate::a2a::Message::builder()
        .role(Role::User)
        .parts(vec![A2aPart::text(text)])
        .message_id(uuid::Uuid::new_v4().to_string())
        .build()
}

fn convert_update_event(
    invocation_id: &str,
    agent_name: &str,
    update: UpdateEvent,
) -> Option<Event> {
    match update {
        UpdateEvent::TaskArtifactUpdate(artifact_event) => {
            let parts: Vec<Part> = artifact_event
                .artifact
                .parts
                .iter()
                .filter_map(|p| match p {
                    A2aPart::Text { text, .. } => Some(Part::Text { text: text.clone() }),
                    _ => None,
                })
                .collect();

            if parts.is_empty() {
                return None;
            }

            let mut event = Event::new(invocation_id.to_string());
            event.author = agent_name.to_string();
            event.llm_response.content = Some(Content { role: "model".to_string(), parts });
            event.llm_response.partial = !artifact_event.last_chunk;
            Some(event)
        }
        UpdateEvent::TaskStatusUpdate(status_event) => {
            // Only create event for final status updates with messages
            if status_event.final_update {
                if let Some(msg) = status_event.status.message {
                    let mut event = Event::new(invocation_id.to_string());
                    event.author = agent_name.to_string();
                    event.llm_response.content = Some(Content {
                        role: "model".to_string(),
                        parts: vec![Part::Text { text: msg }],
                    });
                    event.llm_response.turn_complete = true;
                    return Some(event);
                }
            }
            None
        }
    }
}

fn create_error_event(invocation_id: &str, agent_name: &str, error: &str) -> Event {
    let mut event = Event::new(invocation_id.to_string());
    event.author = agent_name.to_string();
    event.llm_response.error_message = Some(error.to_string());
    event.llm_response.turn_complete = true;
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder() {
        let agent = RemoteA2aAgent::builder("test")
            .description("Test agent")
            .agent_url("http://localhost:8080")
            .build()
            .unwrap();

        assert_eq!(agent.name(), "test");
        assert_eq!(agent.description(), "Test agent");
    }

    #[test]
    fn test_builder_missing_url() {
        let result = RemoteA2aAgent::builder("test").build();
        assert!(result.is_err());
    }
}

// ── A2A v1.0.0 Remote Agent ─────────────────────────────────────────────────

#[cfg(feature = "a2a-v1")]
pub mod v1_remote {
    //! V1.0.0 remote agent wrapper.
    //!
    //! Implements the ADK [`Agent`] trait by communicating with a remote A2A
    //! v1.0.0 agent. Parses the v1 [`AgentCard`] structure including
    //! `supportedInterfaces`, selects the best interface URL (preferring
    //! JSONRPC over HTTP+JSON), and uses [`A2aV1Client`] which sends the
    //! `A2A-Version: 1.0` header on all requests.

    use crate::a2a::client::v1_client::A2aV1Client;
    use a2a_protocol_types::{AgentCard, AgentInterface};
    use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Part, Result};
    use async_trait::async_trait;
    use std::sync::Arc;

    /// Configuration for a v1.0.0 remote A2A agent.
    #[derive(Clone)]
    pub struct RemoteA2aV1Config {
        /// Name of the agent.
        pub name: String,
        /// Description of the agent.
        pub description: String,
        /// The v1.0.0 agent card describing the remote agent.
        pub agent_card: AgentCard,
    }

    /// An agent that communicates with a remote A2A v1.0.0 agent.
    ///
    /// Selects the best interface URL from the agent card's
    /// `supportedInterfaces` (preferring JSONRPC, falling back to HTTP+JSON)
    /// and delegates to [`A2aV1Client`] for protocol-level communication.
    pub struct RemoteA2aV1Agent {
        config: RemoteA2aV1Config,
    }

    impl RemoteA2aV1Agent {
        /// Creates a new v1 remote agent from the given configuration.
        pub fn new(config: RemoteA2aV1Config) -> Self {
            Self { config }
        }

        /// Selects the best interface from the agent card.
        ///
        /// Prefers JSONRPC, falls back to HTTP+JSON.
        pub fn select_interface(card: &AgentCard) -> Option<&AgentInterface> {
            card.supported_interfaces.iter().find(|i| i.protocol_binding == "JSONRPC").or_else(
                || card.supported_interfaces.iter().find(|i| i.protocol_binding == "HTTP+JSON"),
            )
        }
    }

    #[async_trait]
    impl Agent for RemoteA2aV1Agent {
        fn name(&self) -> &str {
            &self.config.name
        }

        fn description(&self) -> &str {
            &self.config.description
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            let card = self.config.agent_card.clone();
            let invocation_id = ctx.invocation_id().to_string();
            let agent_name = self.config.name.clone();

            // Get user content from context
            let user_content = extract_user_text(ctx.as_ref());

            let stream = async_stream::stream! {
                // Verify we have a usable interface
                let interface = match Self::select_interface(&card) {
                    Some(i) => i.clone(),
                    None => {
                        yield Ok(create_v1_error_event(
                            &invocation_id,
                            &agent_name,
                            "no supported interface found in agent card (need JSONRPC or HTTP+JSON)",
                        ));
                        return;
                    }
                };

                // Build a card with the selected interface for the client
                let client = A2aV1Client::new(card.clone());

                // Build a v1 Message from user content
                let message = build_v1_message(user_content);

                // Send streaming message and process the SSE response
                match client.send_streaming_message(message).await {
                    Ok(response) => {
                        use futures::StreamExt;

                        let mut bytes_stream = response.bytes_stream();
                        let mut buffer = String::new();

                        while let Some(chunk_result) = bytes_stream.next().await {
                            let chunk = match chunk_result {
                                Ok(c) => c,
                                Err(e) => {
                                    yield Ok(create_v1_error_event(
                                        &invocation_id,
                                        &agent_name,
                                        &format!("stream error: {e}"),
                                    ));
                                    break;
                                }
                            };

                            buffer.push_str(&String::from_utf8_lossy(&chunk));

                            // Process complete SSE events (delimited by \n\n)
                            while let Some(event_end) = buffer.find("\n\n") {
                                let event_data = buffer[..event_end].to_string();
                                buffer = buffer[event_end + 2..].to_string();

                                if let Some(data) = parse_sse_data_line(&event_data) {
                                    if data.is_empty() {
                                        continue;
                                    }

                                    // Parse as StreamResponse (may be wrapped in JSON-RPC or direct)
                                    if let Some(event) = parse_stream_response(
                                        &data,
                                        &invocation_id,
                                        &agent_name,
                                    ) {
                                        yield Ok(event);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        yield Ok(create_v1_error_event(
                            &invocation_id,
                            &agent_name,
                            &format!("failed to send streaming message: {e}"),
                        ));
                    }
                }

                let _ = interface;
            };

            Ok(Box::pin(stream))
        }
    }

    /// Extracts user text from the invocation context.
    fn extract_user_text(ctx: &dyn InvocationContext) -> Option<String> {
        let content = ctx.user_content();
        for part in &content.parts {
            if let Part::Text { text } = part {
                return Some(text.clone());
            }
        }
        None
    }

    /// Builds a v1 `Message` from user text content.
    fn build_v1_message(content: Option<String>) -> a2a_protocol_types::Message {
        let text = content.unwrap_or_default();
        a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new(uuid::Uuid::new_v4().to_string()),
            role: a2a_protocol_types::MessageRole::User,
            parts: vec![a2a_protocol_types::Part::text(text)],
            task_id: None,
            context_id: None,
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    /// Parses the `data:` field from an SSE event line.
    fn parse_sse_data_line(event: &str) -> Option<String> {
        for line in event.lines() {
            if let Some(data) = line.strip_prefix("data:") {
                return Some(data.trim().to_string());
            }
        }
        None
    }

    /// Attempts to parse an SSE data payload as a StreamResponse (either
    /// direct JSON or wrapped in a JSON-RPC response) and converts it to
    /// an ADK Event.
    fn parse_stream_response(data: &str, invocation_id: &str, agent_name: &str) -> Option<Event> {
        use a2a_protocol_types::events::StreamResponse;

        // Try direct StreamResponse first (REST binding)
        if let Ok(stream_resp) = serde_json::from_str::<StreamResponse>(data) {
            return convert_stream_response(&stream_resp, invocation_id, agent_name);
        }

        // Try JSON-RPC wrapped response
        if let Ok(rpc_value) = serde_json::from_str::<serde_json::Value>(data) {
            if let Some(result) = rpc_value.get("result") {
                if let Ok(stream_resp) = serde_json::from_value::<StreamResponse>(result.clone()) {
                    return convert_stream_response(&stream_resp, invocation_id, agent_name);
                }
            }
            // Check for JSON-RPC error
            if let Some(error) = rpc_value.get("error") {
                let message =
                    error.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error");
                let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                return Some(create_v1_error_event(
                    invocation_id,
                    agent_name,
                    &format!("RPC error {code}: {message}"),
                ));
            }
        }

        tracing::debug!("failed to parse SSE data as StreamResponse: {data}");
        None
    }

    /// Converts a `StreamResponse` into an ADK `Event`.
    fn convert_stream_response(
        resp: &a2a_protocol_types::events::StreamResponse,
        invocation_id: &str,
        agent_name: &str,
    ) -> Option<Event> {
        use a2a_protocol_types::events::StreamResponse;

        match resp {
            StreamResponse::ArtifactUpdate(artifact_event) => {
                use a2a_protocol_types::PartContent;
                let parts: Vec<Part> = artifact_event
                    .artifact
                    .parts
                    .iter()
                    .filter_map(|p| match &p.content {
                        PartContent::Text(text) => Some(Part::Text { text: text.clone() }),
                        _ => None,
                    })
                    .collect();

                if parts.is_empty() {
                    return None;
                }

                let mut event = Event::new(invocation_id.to_string());
                event.author = agent_name.to_string();
                event.llm_response.content = Some(Content { role: "model".to_string(), parts });
                event.llm_response.partial = !artifact_event.last_chunk.unwrap_or(true);
                Some(event)
            }
            StreamResponse::StatusUpdate(status_event) => {
                // In v1, the message field on TaskStatus is a Message object
                // (not a String like in v0.3). Extract text from its parts.
                let is_terminal = matches!(
                    status_event.status.state,
                    a2a_protocol_types::task::TaskState::Completed
                        | a2a_protocol_types::task::TaskState::Failed
                        | a2a_protocol_types::task::TaskState::Canceled
                        | a2a_protocol_types::task::TaskState::Rejected
                );

                if let Some(ref msg) = status_event.status.message {
                    use a2a_protocol_types::PartContent;
                    let text_parts: Vec<Part> = msg
                        .parts
                        .iter()
                        .filter_map(|p| match &p.content {
                            PartContent::Text(text) => Some(Part::Text { text: text.clone() }),
                            _ => None,
                        })
                        .collect();

                    if !text_parts.is_empty() {
                        let mut event = Event::new(invocation_id.to_string());
                        event.author = agent_name.to_string();
                        event.llm_response.content =
                            Some(Content { role: "model".to_string(), parts: text_parts });
                        event.llm_response.turn_complete = is_terminal;
                        return Some(event);
                    }
                }

                // For terminal states without a message, emit a turn-complete event
                if is_terminal {
                    let mut event = Event::new(invocation_id.to_string());
                    event.author = agent_name.to_string();
                    event.llm_response.turn_complete = true;
                    return Some(event);
                }

                None
            }
            // Task and Message variants — emit text if available
            _ => None,
        }
    }

    /// Creates an error event for the v1 remote agent.
    fn create_v1_error_event(invocation_id: &str, agent_name: &str, error: &str) -> Event {
        let mut event = Event::new(invocation_id.to_string());
        event.author = agent_name.to_string();
        event.llm_response.error_message = Some(error.to_string());
        event.llm_response.turn_complete = true;
        event
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use a2a_protocol_types::{AgentCapabilities, AgentInterface, AgentSkill};

        fn make_test_card() -> AgentCard {
            AgentCard {
                name: "test-v1-agent".to_string(),
                url: Some("http://localhost:9999".to_string()),
                description: "A test v1 agent".to_string(),
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

        #[test]
        fn select_interface_prefers_jsonrpc() {
            let card = make_test_card();
            let selected = RemoteA2aV1Agent::select_interface(&card).unwrap();
            assert_eq!(selected.protocol_binding, "JSONRPC");
            assert_eq!(selected.url, "http://localhost:9999/a2a");
        }

        #[test]
        fn select_interface_falls_back_to_http_json() {
            let mut card = make_test_card();
            card.supported_interfaces.retain(|i| i.protocol_binding != "JSONRPC");
            let selected = RemoteA2aV1Agent::select_interface(&card).unwrap();
            assert_eq!(selected.protocol_binding, "HTTP+JSON");
            assert_eq!(selected.url, "http://localhost:9999/rest");
        }

        #[test]
        fn select_interface_returns_none_for_unsupported() {
            let mut card = make_test_card();
            card.supported_interfaces = vec![AgentInterface {
                url: "grpc://localhost:9999".to_string(),
                protocol_binding: "GRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            }];
            assert!(RemoteA2aV1Agent::select_interface(&card).is_none());
        }

        #[test]
        fn select_interface_returns_none_for_empty() {
            let mut card = make_test_card();
            card.supported_interfaces = vec![];
            assert!(RemoteA2aV1Agent::select_interface(&card).is_none());
        }

        #[test]
        fn new_agent_stores_config() {
            let card = make_test_card();
            let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
                name: "my-agent".to_string(),
                description: "My remote agent".to_string(),
                agent_card: card,
            });
            assert_eq!(agent.name(), "my-agent");
            assert_eq!(agent.description(), "My remote agent");
        }

        #[test]
        fn agent_has_no_sub_agents() {
            let card = make_test_card();
            let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
                name: "test".to_string(),
                description: "test".to_string(),
                agent_card: card,
            });
            assert!(agent.sub_agents().is_empty());
        }

        #[test]
        fn build_v1_message_with_content() {
            let msg = build_v1_message(Some("hello".to_string()));
            assert_eq!(msg.role, a2a_protocol_types::MessageRole::User);
            assert_eq!(msg.parts.len(), 1);
            assert_eq!(msg.parts[0].text_content(), Some("hello"));
        }

        #[test]
        fn build_v1_message_without_content() {
            let msg = build_v1_message(None);
            assert_eq!(msg.parts[0].text_content(), Some(""));
        }

        #[test]
        fn parse_sse_data_line_extracts_data() {
            let event = "event: message\ndata: {\"test\": true}\n";
            assert_eq!(parse_sse_data_line(event), Some("{\"test\": true}".to_string()));
        }

        #[test]
        fn parse_sse_data_line_returns_none_without_data() {
            let event = "event: ping\n";
            assert!(parse_sse_data_line(event).is_none());
        }

        #[test]
        fn convert_status_update_with_message() {
            use a2a_protocol_types::events::TaskStatusUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let mut status = TaskStatus::new(TaskState::Completed);
            status.message = Some(a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new("msg-1"),
                role: a2a_protocol_types::MessageRole::Agent,
                parts: vec![a2a_protocol_types::Part::text("done!")],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            });

            let status_event = TaskStatusUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                status,
                metadata: None,
            };

            let resp = a2a_protocol_types::events::StreamResponse::StatusUpdate(status_event);
            let event = convert_stream_response(&resp, "inv-1", "agent-1").unwrap();

            assert_eq!(event.author, "agent-1");
            assert!(event.llm_response.turn_complete);
            let content = event.llm_response.content.unwrap();
            assert_eq!(content.parts.len(), 1);
            match &content.parts[0] {
                Part::Text { text } => assert_eq!(text, "done!"),
                _ => panic!("expected text part"),
            }
        }

        #[test]
        fn convert_status_update_terminal_without_message() {
            use a2a_protocol_types::events::TaskStatusUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let status_event = TaskStatusUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                status: TaskStatus::new(TaskState::Failed),
                metadata: None,
            };

            let resp = a2a_protocol_types::events::StreamResponse::StatusUpdate(status_event);
            let event = convert_stream_response(&resp, "inv-1", "agent-1").unwrap();

            assert!(event.llm_response.turn_complete);
            assert!(event.llm_response.content.is_none());
        }

        #[test]
        fn convert_status_update_non_terminal_without_message() {
            use a2a_protocol_types::events::TaskStatusUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let status_event = TaskStatusUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                status: TaskStatus::new(TaskState::Working),
                metadata: None,
            };

            let resp = a2a_protocol_types::events::StreamResponse::StatusUpdate(status_event);
            let result = convert_stream_response(&resp, "inv-1", "agent-1");

            // Non-terminal without message produces no event
            assert!(result.is_none());
        }

        #[test]
        fn convert_artifact_update_with_text() {
            use a2a_protocol_types::artifact::{Artifact, ArtifactId};
            use a2a_protocol_types::events::TaskArtifactUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId};

            let artifact_event = TaskArtifactUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                artifact: Artifact {
                    id: ArtifactId::new("art-1"),
                    name: Some("result".to_string()),
                    description: None,
                    parts: vec![a2a_protocol_types::Part::text("artifact content")],
                    extensions: None,
                    metadata: None,
                },
                append: None,
                last_chunk: Some(true),
                metadata: None,
            };

            let resp = a2a_protocol_types::events::StreamResponse::ArtifactUpdate(artifact_event);
            let event = convert_stream_response(&resp, "inv-1", "agent-1").unwrap();

            assert_eq!(event.author, "agent-1");
            let content = event.llm_response.content.unwrap();
            assert_eq!(content.parts.len(), 1);
            match &content.parts[0] {
                Part::Text { text } => assert_eq!(text, "artifact content"),
                _ => panic!("expected text part"),
            }
            // last_chunk=true means partial=false
            assert!(!event.llm_response.partial);
        }

        #[test]
        fn convert_artifact_update_partial() {
            use a2a_protocol_types::artifact::{Artifact, ArtifactId};
            use a2a_protocol_types::events::TaskArtifactUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId};

            let artifact_event = TaskArtifactUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                artifact: Artifact {
                    id: ArtifactId::new("art-1"),
                    name: None,
                    description: None,
                    parts: vec![a2a_protocol_types::Part::text("partial...")],
                    extensions: None,
                    metadata: None,
                },
                append: None,
                last_chunk: Some(false),
                metadata: None,
            };

            let resp = a2a_protocol_types::events::StreamResponse::ArtifactUpdate(artifact_event);
            let event = convert_stream_response(&resp, "inv-1", "agent-1").unwrap();

            assert!(event.llm_response.partial);
        }

        #[test]
        fn create_v1_error_event_sets_fields() {
            let event = create_v1_error_event("inv-1", "agent-1", "something broke");
            assert_eq!(event.author, "agent-1");
            assert_eq!(event.llm_response.error_message.as_deref(), Some("something broke"));
            assert!(event.llm_response.turn_complete);
        }
    }
}
