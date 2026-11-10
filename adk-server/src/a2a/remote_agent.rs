use crate::a2a::{
    A2aClient, Part as A2aPart, Role, TaskArtifactUpdateEvent, TaskStatusUpdateEvent, UpdateEvent,
};
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
    /// Base URL of the remote agent, such as `http://localhost:8080`.
    ///
    /// The agent card is fetched from `{agent_url}/.well-known/agent-card.json`,
    /// falling back to `{agent_url}/.well-known/agent.json` when that returns 404.
    pub agent_url: String,
    /// Whether to use streaming for communication.
    /// If `None`, the agent uses streaming if the remote agent supports it.
    pub streaming: Option<bool>,
    /// Mints the bearer token sent to the remote agent, for the running agent's
    /// [`ActingContext`](adk_core::ActingContext) and with `agent_url` as the resource.
    /// `None` sends no credential.
    pub credential_broker: Option<Arc<dyn adk_core::CredentialBroker>>,
    /// Scopes requested from the [`credential_broker`](Self::credential_broker).
    pub credential_scopes: Vec<String>,
    /// Origins besides `agent_url`'s own that the agent card may name as its endpoint;
    /// see [`check_card_endpoint`](crate::a2a::client::check_card_endpoint). Empty by
    /// default, so a card naming another origin is refused.
    pub allowed_endpoint_origins: Vec<String>,
}

/// Mints a token for `acting` at `resource` with `scopes`.
async fn brokered_token(
    broker: &dyn adk_core::CredentialBroker,
    acting: &adk_core::ActingContext,
    resource: &str,
    scopes: &[String],
) -> Result<adk_core::ScopedToken> {
    let resource = adk_core::resource_indicator(resource)?;
    broker.token_for(acting, &resource, scopes).await
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
        let config_streaming = self.config.streaming;
        let broker = self.config.credential_broker.clone();
        let scopes = self.config.credential_scopes.clone();
        let allowed_origins = self.config.allowed_endpoint_origins.clone();
        let acting = ctx.acting_context();

        // Get user content from context
        let user_content = get_user_content_from_context(ctx.as_ref());

        let stream = async_stream::stream! {
            // Create A2A client from a card whose endpoint is on an expected origin
            let mut client = match A2aClient::resolve_agent_card_allowing(&url, &allowed_origins).await {
                Ok(card) => A2aClient::new(card),
                Err(e) => {
                    yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                    return;
                }
            };
            // The token's audience is the configured agent URL, so a card pointing the
            // endpoint at another host cannot receive it.
            if let Some(broker) = broker.as_deref() {
                match brokered_token(broker, &acting, &url, &scopes).await {
                    Ok(token) => client = client.with_credential(token),
                    Err(e) => {
                        yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                        return;
                    }
                }
            }

            // Determine if we should use streaming
            let use_streaming = config_streaming.unwrap_or(client.agent_card().capabilities.streaming);

            // Build message from user content
            let message = build_a2a_message(user_content);

            if use_streaming {
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
            } else {
                // Send non-streaming message
                match client.send_message(message).await {
                    Ok(rpc_response) => {
                        if let Some(result) = rpc_response.result {
                            match serde_json::from_value::<crate::a2a::Task>(result) {
                                Ok(task) => {
                                    for event in convert_task_to_events(&invocation_id, &agent_name, task) {
                                        yield Ok(event);
                                    }
                                }
                                Err(e) => {
                                    yield Ok(create_error_event(&invocation_id, &agent_name, &format!("failed to parse response: {e}")));
                                }
                            }
                        } else if let Some(error) = rpc_response.error {
                            yield Ok(create_error_event(&invocation_id, &agent_name, &format!("RPC error: {} ({})", error.message, error.code)));
                        }
                    }
                    Err(e) => {
                        yield Ok(create_error_event(&invocation_id, &agent_name, &e.to_string()));
                    }
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
    streaming: Option<bool>,
    credential_broker: Option<Arc<dyn adk_core::CredentialBroker>>,
    credential_scopes: Vec<String>,
    allowed_endpoint_origins: Vec<String>,
}

impl RemoteA2aAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            agent_url: None,
            streaming: None,
            credential_broker: None,
            credential_scopes: Vec::new(),
            allowed_endpoint_origins: Vec::new(),
        }
    }

    /// Accepts an agent card endpoint on `origin`, such as `https://rpc.example.com`, in
    /// addition to the agent URL's own origin.
    ///
    /// See [`RemoteA2aConfig::allowed_endpoint_origins`].
    pub fn allow_endpoint_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_endpoint_origins.push(origin.into());
        self
    }

    /// Authenticates to the remote agent with a token from `broker`, minted for the
    /// running agent's acting context with the agent URL as the resource.
    ///
    /// See [`RemoteA2aConfig::credential_broker`].
    pub fn credential_broker(
        mut self,
        broker: Arc<dyn adk_core::CredentialBroker>,
        scopes: Vec<String>,
    ) -> Self {
        self.credential_broker = Some(broker);
        self.credential_scopes = scopes;
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn agent_url(mut self, url: impl Into<String>) -> Self {
        self.agent_url = Some(url.into());
        self
    }

    /// Set whether to use streaming. If not set, auto-detects from agent capabilities.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = Some(streaming);
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
            streaming: self.streaming,
            credential_broker: self.credential_broker,
            credential_scopes: self.credential_scopes,
            allowed_endpoint_origins: self.allowed_endpoint_origins,
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
            if status_event.final_update
                && let Some(msg) = status_event.status.message
            {
                let mut event = Event::new(invocation_id.to_string());
                event.author = agent_name.to_string();
                event.llm_response.content = Some(Content {
                    role: "model".to_string(),
                    parts: vec![Part::Text { text: msg }],
                });
                event.llm_response.turn_complete = true;
                return Some(event);
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

/// Converts a non-streaming A2A Task response into a sequence of ADK Events.
fn convert_task_to_events(
    invocation_id: &str,
    agent_name: &str,
    task: crate::a2a::Task,
) -> Vec<Event> {
    let mut events = Vec::new();

    // 1. Yield events for artifacts
    if let Some(artifacts) = task.artifacts {
        for artifact in artifacts {
            let update = UpdateEvent::TaskArtifactUpdate(TaskArtifactUpdateEvent {
                task_id: task.id.clone(),
                context_id: task.context_id.clone(),
                artifact,
                append: false,
                last_chunk: true,
            });
            if let Some(event) = convert_update_event(invocation_id, agent_name, update) {
                events.push(event);
            }
        }
    }

    // 2. Yield events for history (agent messages)
    if let Some(history) = task.history {
        for msg in history {
            if msg.role == Role::Agent {
                let parts: Vec<Part> = msg
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        A2aPart::Text { text, .. } => Some(Part::Text { text: text.clone() }),
                        _ => None,
                    })
                    .collect();

                if !parts.is_empty() {
                    let mut event = Event::new(invocation_id.to_string());
                    event.author = agent_name.to_string();
                    event.llm_response.content = Some(Content { role: "model".to_string(), parts });
                    event.llm_response.turn_complete = false;
                    events.push(event);
                }
            }
        }
    }

    // 3. Yield final status event
    let update = UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
        task_id: task.id,
        context_id: task.context_id,
        status: task.status,
        final_update: true,
    });
    if let Some(event) = convert_update_event(invocation_id, agent_name, update) {
        events.push(event);
    }

    events
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

    #[test]
    fn test_builder_streaming_option() {
        let agent = RemoteA2aAgent::builder("test")
            .agent_url("http://localhost:8080")
            .streaming(true)
            .build()
            .unwrap();

        assert_eq!(agent.config.streaming, Some(true));
    }

    #[test]
    fn test_builder_streaming_default_is_none() {
        let agent =
            RemoteA2aAgent::builder("test").agent_url("http://localhost:8080").build().unwrap();

        assert_eq!(agent.config.streaming, None);
    }

    #[test]
    fn test_convert_task_to_events_with_artifacts_and_history() {
        use crate::a2a::{Artifact, Message, Task, TaskState, TaskStatus};

        let task = Task {
            id: "task-123".to_string(),
            context_id: Some("ctx-456".to_string()),
            status: TaskStatus { state: TaskState::Completed, message: Some("Done".to_string()) },
            artifacts: Some(vec![Artifact {
                artifact_id: "art-1".to_string(),
                name: Some("result".to_string()),
                description: None,
                parts: vec![A2aPart::text("artifact content".to_string())],
                metadata: None,
                extensions: None,
            }]),
            history: Some(vec![
                Message::builder()
                    .role(Role::Agent)
                    .parts(vec![A2aPart::text("Hello from agent".to_string())])
                    .build(),
            ]),
        };

        let events = convert_task_to_events("inv-1", "remote-agent", task);

        // Should have 3 events: artifact, history message, final status
        assert_eq!(events.len(), 3);

        // Artifact event
        assert_eq!(events[0].author, "remote-agent");
        let content = events[0].llm_response.content.as_ref().unwrap();
        assert_eq!(content.parts[0], Part::Text { text: "artifact content".to_string() });

        // History event
        assert_eq!(events[1].author, "remote-agent");
        let content = events[1].llm_response.content.as_ref().unwrap();
        assert_eq!(content.parts[0], Part::Text { text: "Hello from agent".to_string() });
        assert!(!events[1].llm_response.turn_complete);

        // Final status event
        assert_eq!(events[2].author, "remote-agent");
        assert!(events[2].llm_response.turn_complete);
    }

    #[test]
    fn test_convert_task_to_events_empty_task() {
        use crate::a2a::{Task, TaskState, TaskStatus};

        let task = Task {
            id: "task-empty".to_string(),
            context_id: None,
            status: TaskStatus { state: TaskState::Completed, message: None },
            artifacts: None,
            history: None,
        };

        let events = convert_task_to_events("inv-1", "agent", task);

        // A completed task with no message and no artifacts produces no events
        // because convert_update_event only emits for final_update with a message
        assert!(events.is_empty());
    }

    #[test]
    fn test_convert_task_to_events_status_with_message() {
        use crate::a2a::{Task, TaskState, TaskStatus};

        let task = Task {
            id: "task-msg".to_string(),
            context_id: None,
            status: TaskStatus {
                state: TaskState::Completed,
                message: Some("All done".to_string()),
            },
            artifacts: None,
            history: None,
        };

        let events = convert_task_to_events("inv-1", "agent", task);

        assert_eq!(events.len(), 1);
        assert!(events[0].llm_response.turn_complete);
        let content = events[0].llm_response.content.as_ref().unwrap();
        assert_eq!(content.parts[0], Part::Text { text: "All done".to_string() });
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
    use a2a_protocol_types::responses::SendMessageResponse;
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
        /// Whether to use streaming for communication.
        /// If `None`, auto-detects from the agent card capabilities.
        pub streaming: Option<bool>,
        /// Mints the bearer token sent to the remote agent, for the running agent's
        /// [`ActingContext`](adk_core::ActingContext) and with the selected interface
        /// URL as the resource. `None` sends no credential.
        pub credential_broker: Option<Arc<dyn adk_core::CredentialBroker>>,
        /// Scopes requested from the [`credential_broker`](Self::credential_broker).
        pub credential_scopes: Vec<String>,
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

        /// Creates a remote agent named `name` for the A2A v1 agent at `base_url`, from the
        /// card it serves at `/.well-known/agent-card.json`. The card's description becomes
        /// the agent's, and streaming follows the card.
        ///
        /// # Errors
        ///
        /// Returns an error when the card cannot be fetched or names an endpoint on
        /// another origin; see [`A2aV1Client::resolve_agent_card`].
        pub async fn from_url(
            name: impl Into<String>,
            base_url: &str,
        ) -> std::result::Result<Self, crate::a2a::client::v1_client::V1ClientError> {
            let agent_card = A2aV1Client::resolve_agent_card(base_url).await?;
            Ok(Self::new(RemoteA2aV1Config {
                name: name.into(),
                description: agent_card.description.clone(),
                agent_card,
                streaming: None,
                credential_broker: None,
                credential_scopes: Vec::new(),
            }))
        }

        /// Selects the best A2A v1 interface from the agent card.
        ///
        /// Prefers JSONRPC, falls back to HTTP+JSON, and never picks an interface for an
        /// earlier protocol version; see [`v1_interface`](crate::a2a::client::v1_client::v1_interface).
        pub fn select_interface(card: &AgentCard) -> Option<&AgentInterface> {
            use crate::a2a::client::v1_client::v1_interface;
            v1_interface(card, "JSONRPC").or_else(|| v1_interface(card, "HTTP+JSON"))
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
            let config_streaming = self.config.streaming;
            let broker = self.config.credential_broker.clone();
            let scopes = self.config.credential_scopes.clone();
            let acting = ctx.acting_context();

            // The remote conversation this agent continues, and a remote task waiting on it.
            let keys = RemoteKeys::for_agent(&agent_name);
            let state = ctx.session().state();
            let remote_context = state.get(&keys.context_id).and_then(string_value);
            let remote_task = state.get(&keys.task_id).and_then(string_value);
            let parts = outgoing_parts(ctx.as_ref(), &agent_name);

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
                let mut client = A2aV1Client::new(card.clone());
                if let Some(broker) = broker.as_deref() {
                    match super::brokered_token(broker, &acting, &interface.url, &scopes).await {
                        Ok(token) => client = client.with_credential(token),
                        Err(e) => {
                            yield Ok(create_v1_error_event(&invocation_id, &agent_name, &e.to_string()));
                            return;
                        }
                    }
                }

                // Determine if we should use streaming
                let use_streaming = config_streaming.unwrap_or(card.capabilities.streaming.unwrap_or(false));

                let message = build_v1_message(parts, remote_context, remote_task);

                if use_streaming {
                    // The remote's updates as they arrive. A stream that stops before a final
                    // status is reported as an error.
                    match client.stream_message(message).await {
                        Ok(mut updates) => {
                            use futures::StreamExt;

                            let mut finished = false;
                            let mut remote = RemoteEvents::new(&invocation_id, &agent_name);
                            while let Some(update) = updates.next().await {
                                let stream_response = match update {
                                    Ok(update) => update,
                                    Err(e) => {
                                        yield Ok(create_v1_error_event(&invocation_id, &agent_name, &e.to_string()));
                                        return;
                                    }
                                };
                                if ends_stream(&stream_response) {
                                    finished = true;
                                    let mut events = remote.handle(&stream_response);
                                    let last = events.pop().unwrap_or_else(|| remote.new_event());
                                    for event in events {
                                        yield Ok(event);
                                    }
                                    yield Ok(end_turn(last, &stream_response, &keys));
                                    // The server closes the stream here (A2A §3.1.2). Read to its
                                    // end so the connection closes cleanly, but not for long.
                                    let drain = async { while updates.next().await.is_some() {} };
                                    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), drain).await;
                                    break;
                                }
                                for event in remote.handle(&stream_response) {
                                    yield Ok(event);
                                }
                            }

                            if !finished {
                                yield Ok(create_v1_error_event(
                                    &invocation_id,
                                    &agent_name,
                                    &crate::a2a::client::truncated_stream_error().to_string(),
                                ));
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
                } else {
                    // Send non-streaming message, over the binding the card offers.
                    let sent = if interface.protocol_binding == "HTTP+JSON" {
                        client.send_message_rest(message).await
                    } else {
                        client.send_message(message).await
                    };
                    match sent {
                        Ok(SendMessageResponse::Message(reply)) => {
                            // The agent answered directly, without a task.
                            let resp = a2a_protocol_types::events::StreamResponse::Message(reply);
                            let mut remote = RemoteEvents::new(&invocation_id, &agent_name);
                            let event = remote.handle(&resp).pop().unwrap_or_else(|| remote.new_event());
                            yield Ok(end_turn(event, &resp, &keys));
                        }
                        Ok(SendMessageResponse::Task(task)) => {
                            // One event for the task: its artifacts and status message.
                            let resp = a2a_protocol_types::events::StreamResponse::Task(task);
                            let mut remote = RemoteEvents::new(&invocation_id, &agent_name);
                            let event = remote.handle(&resp).pop().unwrap_or_else(|| remote.new_event());
                            yield Ok(end_turn(event, &resp, &keys));
                        }
                        Ok(_) => {
                            yield Ok(create_v1_error_event(
                                &invocation_id,
                                &agent_name,
                                "unsupported SendMessage result: neither a task nor a message",
                            ));
                        }
                        Err(e) => {
                            yield Ok(create_v1_error_event(
                                &invocation_id,
                                &agent_name,
                                &format!("failed to send message: {e}"),
                            ));
                        }
                    }
                }
            };

            Ok(Box::pin(stream))
        }
    }

    /// Session state keys under which an agent keeps its remote conversation.
    struct RemoteKeys {
        /// The remote `contextId` the agent continues.
        context_id: String,
        /// The remote task waiting on this side, while there is one.
        task_id: String,
    }

    impl RemoteKeys {
        fn for_agent(agent_name: &str) -> Self {
            Self {
                context_id: format!("a2a_remote:{agent_name}:context_id"),
                task_id: format!("a2a_remote:{agent_name}:task_id"),
            }
        }
    }

    fn string_value(value: serde_json::Value) -> Option<String> {
        value.as_str().filter(|s| !s.is_empty()).map(str::to_string)
    }

    /// What this turn sends: the session since this agent's last reply (the person's
    /// messages, and other agents' turns as the context the session renders them as), or
    /// the turn's own user content when the session holds none. Each part goes out in the
    /// shape every A2A v1 SDK reads; see [`outgoing_part`].
    fn outgoing_parts(
        ctx: &dyn InvocationContext,
        agent_name: &str,
    ) -> Vec<a2a_protocol_types::Part> {
        let history = ctx.session().conversation_history_with_context(agent_name, ctx.branch());
        let since_reply =
            history.iter().rposition(|content| content.role == "model").map_or(0, |i| i + 1);
        let mut parts: Vec<_> = history[since_reply..]
            .iter()
            .flat_map(|content| &content.parts)
            .filter_map(outgoing_part)
            .collect();
        if parts.is_empty() {
            parts = ctx.user_content().parts.iter().filter_map(outgoing_part).collect();
        }
        parts
    }

    /// An ADK part as A2A sends it:
    /// - text as text;
    /// - inline data and files as raw bytes and URLs;
    /// - function calls and responses as data parts tagged `adk_type`, the shape
    ///   adk-python, adk-go and adk-server read, so an answer to a remote long-running call
    ///   resumes it.
    ///
    /// Thinking and provider items stay here.
    fn outgoing_part(part: &Part) -> Option<a2a_protocol_types::Part> {
        use crate::a2a::v1::convert::{
            media_part_to_wire, tagged_function_call_part, tagged_function_response_part,
        };
        match part {
            Part::Text { text } if !text.trim().is_empty() => {
                Some(a2a_protocol_types::Part::text(text.clone()))
            }
            Part::FunctionCall { name, args, id, .. } => {
                Some(tagged_function_call_part(name, args, id.as_deref(), false))
            }
            Part::FunctionResponse { function_response, id, .. } => {
                Some(tagged_function_response_part(
                    &function_response.name,
                    &function_response.response,
                    id.as_deref(),
                ))
            }
            other => media_part_to_wire(other),
        }
    }

    /// Builds the v1 `Message` for a turn. `context_id` continues the remote conversation,
    /// and `task_id` continues a remote task waiting on this side.
    fn build_v1_message(
        parts: Vec<a2a_protocol_types::Part>,
        context_id: Option<String>,
        task_id: Option<String>,
    ) -> a2a_protocol_types::Message {
        let parts = if parts.is_empty() { vec![a2a_protocol_types::Part::text("")] } else { parts };
        a2a_protocol_types::Message {
            id: a2a_protocol_types::MessageId::new(uuid::Uuid::new_v4().to_string()),
            role: a2a_protocol_types::MessageRole::User,
            parts,
            task_id: task_id.map(a2a_protocol_types::TaskId::new),
            context_id: context_id.map(a2a_protocol_types::ContextId::new),
            reference_task_ids: None,
            extensions: None,
            metadata: None,
        }
    }

    /// Turns one remote turn's A2A events into ADK events, as adk-go's remote agent does
    /// (`adka2a.ToSessionEvent` and its run processor):
    ///
    /// - an artifact update is a model event with every part of the artifact, partial when
    ///   the server marks it `adk_partial`, otherwise when it appends or is not the last
    ///   chunk (`lastChunk` absent means false, as A2A servers omit it); the server's reset
    ///   of its temporary partial artifact (`adk_partial` with `lastChunk`) is dropped;
    /// - chunks without `adk_partial` are joined per artifact into one complete event when
    ///   their last chunk or the terminal status arrives, so the session keeps the whole
    ///   answer; a `Task` snapshot starts over;
    /// - a status message before the task ends is the remote's reasoning: partial thoughts;
    /// - a final status carries its message, the calls the remote waits on as long-running
    ///   calls, and for `FAILED` the error text (the first part, marked
    ///   `adk_is_error_message`, or `a2a task failed`);
    /// - every event carries the remote task and context (`a2a:task_id`,
    ///   `a2a:context_id` in its metadata), and the update's `adk_escalate`,
    ///   `adk_error_code`, usage, citations and custom metadata.
    struct RemoteEvents {
        invocation_id: String,
        agent_name: String,
        /// Chunks joined by artifact ID, in the order they were last updated.
        aggregations: Vec<(String, Aggregation)>,
    }

    #[derive(Default)]
    struct Aggregation {
        parts: Vec<Part>,
        citations: Vec<adk_core::CitationSource>,
        custom: std::collections::HashMap<String, String>,
        usage: Option<adk_core::UsageMetadata>,
    }

    impl RemoteEvents {
        fn new(invocation_id: &str, agent_name: &str) -> Self {
            Self {
                invocation_id: invocation_id.to_string(),
                agent_name: agent_name.to_string(),
                aggregations: Vec::new(),
            }
        }

        /// The ADK events `response` produces, in order.
        fn handle(&mut self, response: &a2a_protocol_types::events::StreamResponse) -> Vec<Event> {
            use a2a_protocol_types::events::StreamResponse;
            let event = self.to_event(response);
            if partial_flag(response).is_some() {
                return event.into_iter().collect();
            }
            match response {
                StreamResponse::StatusUpdate(update) if is_final_state(update.status.state) => {
                    let mut events: Vec<Event> = std::mem::take(&mut self.aggregations)
                        .into_iter()
                        .map(|(_, aggregation)| self.joined(aggregation))
                        .collect();
                    events.extend(event);
                    events
                }
                StreamResponse::Task(_) => {
                    self.aggregations.clear();
                    event.into_iter().collect()
                }
                StreamResponse::ArtifactUpdate(update) => {
                    let Some(mut event) = event else { return Vec::new() };
                    let id = update.artifact.id.0.clone();
                    let last_chunk = update.last_chunk.unwrap_or(false);
                    if update.append != Some(true) {
                        self.aggregations.retain(|(existing, _)| *existing != id);
                        if last_chunk {
                            event.llm_response.partial = false;
                            return vec![event];
                        }
                    }
                    let mut aggregation = self
                        .aggregations
                        .iter()
                        .position(|(existing, _)| *existing == id)
                        .map(|index| self.aggregations.remove(index).1)
                        .unwrap_or_default();
                    aggregate(&mut aggregation, &event);
                    if !last_chunk {
                        self.aggregations.push((id, aggregation));
                        return vec![event];
                    }
                    let joined = self.joined(aggregation);
                    vec![event, joined]
                }
                _ => event.into_iter().collect(),
            }
        }

        fn joined(&self, aggregation: Aggregation) -> Event {
            let mut event = self.new_event();
            event.llm_response.content =
                Some(Content { role: "model".to_string(), parts: aggregation.parts });
            if !aggregation.citations.is_empty() {
                event.llm_response.citation_metadata =
                    Some(adk_core::CitationMetadata { citation_sources: aggregation.citations });
            }
            event.llm_response.usage_metadata = aggregation.usage;
            event.provider_metadata = aggregation.custom;
            event
        }

        fn new_event(&self) -> Event {
            let mut event = Event::new(self.invocation_id.clone());
            event.author = self.agent_name.clone();
            event
        }

        /// The event one A2A update stands for, if any (adk-go's `ToSessionEvent`).
        fn to_event(&self, response: &a2a_protocol_types::events::StreamResponse) -> Option<Event> {
            use a2a_protocol_types::events::StreamResponse;
            use a2a_protocol_types::task::TaskState;
            match response {
                StreamResponse::Task(task) => {
                    let mut parts = Vec::new();
                    for artifact in task.artifacts.iter().flatten() {
                        parts.extend(adk_parts(&artifact.parts));
                    }
                    let status = status_message(task.status.state, task.status.message.as_ref());
                    parts.extend(status.parts);
                    let final_state = is_final_state(task.status.state);
                    if parts.is_empty() && !final_state {
                        return None;
                    }
                    let mut event = self.new_event();
                    if !parts.is_empty() {
                        if waits(task.status.state) {
                            event.long_running_tool_ids =
                                long_running_ids(&parts, &task_parts(task));
                        }
                        event.llm_response.content =
                            Some(Content { role: "model".to_string(), parts });
                    }
                    event.llm_response.error_message = status.error;
                    apply_metadata(
                        &mut event,
                        task.metadata.as_ref(),
                        None,
                        Some((&task.id.0, &task.context_id.0)),
                    );
                    event.llm_response.turn_complete = final_state;
                    Some(event)
                }
                StreamResponse::Message(message) => {
                    let parts = adk_parts(&message.parts);
                    let mut event = self.new_event();
                    if !parts.is_empty() {
                        let role = if message.role == a2a_protocol_types::MessageRole::User {
                            "user"
                        } else {
                            "model"
                        };
                        event.llm_response.content =
                            Some(Content { role: role.to_string(), parts });
                    }
                    let ids = message
                        .task_id
                        .as_ref()
                        .map(|t| t.0.as_str())
                        .zip(message.context_id.as_ref().map(|c| c.0.as_str()));
                    apply_metadata(&mut event, message.metadata.as_ref(), None, ids);
                    event.llm_response.turn_complete = true;
                    Some(event)
                }
                StreamResponse::ArtifactUpdate(update) => {
                    let partial_flag = partial_flag(response);
                    let last_chunk = update.last_chunk.unwrap_or(false);
                    if update.artifact.parts.is_empty()
                        || (partial_flag == Some(true) && last_chunk)
                    {
                        return None;
                    }
                    let parts = adk_parts(&update.artifact.parts);
                    if parts.is_empty() {
                        return None;
                    }
                    let mut event = self.new_event();
                    event.long_running_tool_ids = long_running_ids(&parts, &update.artifact.parts);
                    event.llm_response.content = Some(Content { role: "model".to_string(), parts });
                    apply_metadata(
                        &mut event,
                        update.metadata.as_ref(),
                        update.artifact.metadata.as_ref(),
                        Some((&update.task_id.0, &update.context_id.0)),
                    );
                    event.llm_response.partial =
                        partial_flag.unwrap_or(update.append == Some(true) || !last_chunk);
                    Some(event)
                }
                StreamResponse::StatusUpdate(update) => {
                    let ids = Some((update.task_id.0.as_str(), update.context_id.0.as_str()));
                    if is_final_state(update.status.state) {
                        let status =
                            status_message(update.status.state, update.status.message.as_ref());
                        let mut event = self.new_event();
                        let wire_parts = update
                            .status
                            .message
                            .as_ref()
                            .map(|m| m.parts.clone())
                            .unwrap_or_default();
                        event.long_running_tool_ids = long_running_ids(&status.parts, &wire_parts);
                        if !status.parts.is_empty() {
                            event.llm_response.content =
                                Some(Content { role: "model".to_string(), parts: status.parts });
                        }
                        event.llm_response.error_message = status.error;
                        apply_metadata(&mut event, update.metadata.as_ref(), None, ids);
                        event.llm_response.turn_complete = true;
                        return Some(event);
                    }
                    let message = update.status.message.as_ref()?;
                    let mut parts = adk_parts(&message.parts);
                    if parts.is_empty() {
                        return None;
                    }
                    // Progress the remote reports while it works is its reasoning.
                    for part in &mut parts {
                        if let Part::Text { text } = part {
                            *part =
                                Part::Thinking { thinking: std::mem::take(text), signature: None };
                        }
                    }
                    let mut event = self.new_event();
                    event.llm_response.content = Some(Content { role: "model".to_string(), parts });
                    apply_metadata(&mut event, update.metadata.as_ref(), None, ids);
                    event.llm_response.partial = true;
                    event.llm_response.turn_complete = false;
                    let _ = TaskState::Working;
                    Some(event)
                }
                _ => None,
            }
        }
    }

    /// The `adk_partial` flag of an update, when the server set one.
    fn partial_flag(response: &a2a_protocol_types::events::StreamResponse) -> Option<bool> {
        use a2a_protocol_types::events::StreamResponse;
        let metadata = match response {
            StreamResponse::ArtifactUpdate(update) => update.metadata.as_ref(),
            StreamResponse::StatusUpdate(update) => update.metadata.as_ref(),
            StreamResponse::Task(task) => task.metadata.as_ref(),
            StreamResponse::Message(message) => message.metadata.as_ref(),
            _ => None,
        };
        metadata
            .and_then(|metadata| metadata.get(crate::a2a::v1::convert::ADK_PARTIAL_METADATA_KEY))
            .and_then(serde_json::Value::as_bool)
    }

    /// Adds `event`'s content and metadata to a chunk aggregation: adjacent text of the same
    /// kind (answer or thought) joins into one part.
    fn aggregate(aggregation: &mut Aggregation, event: &Event) {
        for part in event.content().into_iter().flat_map(|content| &content.parts) {
            match (part, aggregation.parts.last_mut()) {
                (Part::Text { text }, Some(Part::Text { text: last })) => last.push_str(text),
                (Part::Thinking { thinking, .. }, Some(Part::Thinking { thinking: last, .. })) => {
                    last.push_str(thinking);
                }
                (part, _) => aggregation.parts.push(part.clone()),
            }
        }
        if let Some(citations) = &event.llm_response.citation_metadata {
            aggregation.citations.extend(citations.citation_sources.iter().cloned());
        }
        aggregation.custom.extend(event.provider_metadata.clone());
        if event.llm_response.usage_metadata.is_some() {
            aggregation.usage = event.llm_response.usage_metadata.clone();
        }
    }

    /// Whether a task in `state` ends the remote turn: it ended, or it waits on its caller.
    fn is_final_state(state: a2a_protocol_types::task::TaskState) -> bool {
        use a2a_protocol_types::task::TaskState;
        matches!(
            state,
            TaskState::Completed
                | TaskState::Failed
                | TaskState::Canceled
                | TaskState::Rejected
                | TaskState::InputRequired
                | TaskState::AuthRequired
        )
    }

    fn waits(state: a2a_protocol_types::task::TaskState) -> bool {
        matches!(
            state,
            a2a_protocol_types::task::TaskState::InputRequired
                | a2a_protocol_types::task::TaskState::AuthRequired
        )
    }

    /// The ADK parts of A2A parts, without the ones that cannot be read and empty text.
    fn adk_parts(parts: &[a2a_protocol_types::Part]) -> Vec<Part> {
        parts
            .iter()
            .filter_map(|part| crate::a2a::v1::convert::wire_part_to_adk(part).ok())
            .filter(|part| !matches!(part, Part::Text { text } if text.is_empty()))
            .collect()
    }

    /// The wire parts of a task's artifacts and status message, in the order
    /// [`RemoteEvents::to_event`] converts them.
    fn task_parts(task: &a2a_protocol_types::Task) -> Vec<a2a_protocol_types::Part> {
        let mut parts: Vec<_> =
            task.artifacts.iter().flatten().flat_map(|artifact| artifact.parts.clone()).collect();
        parts.extend(task.status.message.iter().flat_map(|message| message.parts.clone()));
        parts
    }

    /// The IDs of the calls among `parts` that the server marks `adk_is_long_running`.
    fn long_running_ids(parts: &[Part], wire: &[a2a_protocol_types::Part]) -> Vec<String> {
        let marked: std::collections::HashSet<String> = wire
            .iter()
            .filter(|part| {
                part.metadata
                    .as_ref()
                    .and_then(|meta| {
                        meta.get(crate::a2a::v1::convert::ADK_LONG_RUNNING_METADATA_KEY)
                    })
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            })
            .filter_map(|part| match &part.content {
                a2a_protocol_types::PartContent::Data(data) => {
                    data.get("id").and_then(serde_json::Value::as_str).map(str::to_owned)
                }
                _ => None,
            })
            .collect();
        parts
            .iter()
            .filter_map(|part| match part {
                Part::FunctionCall { id: Some(id), .. } if marked.contains(id) => Some(id.clone()),
                _ => None,
            })
            .collect()
    }

    /// A status message's content, and the error text of a `FAILED` status.
    struct StatusMessage {
        parts: Vec<Part>,
        error: Option<String>,
    }

    fn status_message(
        state: a2a_protocol_types::task::TaskState,
        message: Option<&a2a_protocol_types::Message>,
    ) -> StatusMessage {
        let failed = state == a2a_protocol_types::task::TaskState::Failed;
        let Some(message) = message else {
            return StatusMessage {
                parts: Vec::new(),
                error: failed.then(|| "a2a task failed".to_string()),
            };
        };
        let mut wire = message.parts.as_slice();
        let mut error = None;
        if failed
            && let Some(first) = wire.first()
            && let Some(text) = first.text_content().filter(|t| !t.is_empty())
        {
            error = Some(text.to_string());
            let marked = first
                .metadata
                .as_ref()
                .and_then(|meta| meta.get(crate::a2a::v1::convert::ADK_ERROR_MESSAGE_METADATA_KEY))
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            if marked {
                wire = &wire[1..];
            }
        }
        if failed && error.is_none() {
            error = Some("a2a task failed".to_string());
        }
        StatusMessage { parts: adk_parts(wire), error }
    }

    /// Reads an update's ADK metadata into `event`, as adk-go's `processA2AMeta`: the
    /// artifact's metadata over the update's, then citations, usage, custom metadata (with
    /// the remote task and context as `a2a:task_id` and `a2a:context_id`), the error code,
    /// and escalation from the update's own metadata.
    fn apply_metadata(
        event: &mut Event,
        update_meta: Option<&serde_json::Value>,
        artifact_meta: Option<&serde_json::Value>,
        ids: Option<(&str, &str)>,
    ) {
        use crate::a2a::v1::processor::{
            ADK_CITATION_METADATA_KEY, ADK_CUSTOM_METADATA_KEY, ADK_ERROR_CODE_METADATA_KEY,
            ADK_ESCALATE_METADATA_KEY, ADK_USAGE_METADATA_KEY,
        };
        let mut meta =
            update_meta.and_then(serde_json::Value::as_object).cloned().unwrap_or_default();
        if let Some(artifact) = artifact_meta.and_then(serde_json::Value::as_object) {
            meta.extend(artifact.clone());
        }
        if let Some(citations) = meta
            .get(ADK_CITATION_METADATA_KEY)
            .and_then(|c| c.get("citations"))
            .and_then(serde_json::Value::as_array)
        {
            let sources = citations
                .iter()
                .map(|citation| {
                    let text = |key: &str| {
                        citation.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
                    };
                    let index = |key: &str| {
                        citation.get(key).and_then(serde_json::Value::as_i64).map(|v| v as i32)
                    };
                    adk_core::CitationSource {
                        uri: text("uri"),
                        title: text("title"),
                        start_index: index("startIndex"),
                        end_index: index("endIndex"),
                        license: text("license"),
                        publication_date: text("publicationDate"),
                    }
                })
                .collect();
            event.llm_response.citation_metadata =
                Some(adk_core::CitationMetadata { citation_sources: sources });
        }
        if let Some(usage) = meta.get(ADK_USAGE_METADATA_KEY) {
            let count =
                |key: &str| usage.get(key).and_then(serde_json::Value::as_i64).map(|v| v as i32);
            event.llm_response.usage_metadata = Some(adk_core::UsageMetadata {
                prompt_token_count: count("promptTokenCount").unwrap_or_default(),
                candidates_token_count: count("candidatesTokenCount").unwrap_or_default(),
                total_token_count: count("totalTokenCount").unwrap_or_default(),
                cache_read_input_token_count: count("cachedContentTokenCount"),
                thinking_token_count: count("thoughtsTokenCount"),
                ..Default::default()
            });
        }
        if let Some((task_id, context_id)) = ids {
            event.provider_metadata.insert(A2A_TASK_ID_KEY.to_string(), task_id.to_string());
            event.provider_metadata.insert(A2A_CONTEXT_ID_KEY.to_string(), context_id.to_string());
        }
        if let Some(custom) =
            meta.get(ADK_CUSTOM_METADATA_KEY).and_then(serde_json::Value::as_object)
        {
            for (key, value) in custom {
                let value = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
                event.provider_metadata.insert(key.clone(), value);
            }
        }
        if let Some(code) =
            meta.get(ADK_ERROR_CODE_METADATA_KEY).and_then(serde_json::Value::as_str)
        {
            event.llm_response.error_code = Some(code.to_string());
        }
        let escalate = update_meta
            .and_then(|meta| meta.get(ADK_ESCALATE_METADATA_KEY))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        event.actions.escalate = escalate;
    }

    /// Event metadata key of the remote task an event came from, as adk-go names it.
    pub const A2A_TASK_ID_KEY: &str = "a2a:task_id";
    /// Event metadata key of the remote context an event came from, as adk-go names it.
    pub const A2A_CONTEXT_ID_KEY: &str = "a2a:context_id";

    /// Ends a remote turn on `event`: saves the remote conversation in session state for
    /// the next turn. The remote task is kept only while it waits on this side.
    fn end_turn(
        mut event: Event,
        resp: &a2a_protocol_types::events::StreamResponse,
        keys: &RemoteKeys,
    ) -> Event {
        use a2a_protocol_types::events::StreamResponse;

        event.llm_response.turn_complete = true;
        let (context_id, task_id, state) = match resp {
            StreamResponse::StatusUpdate(update) => (
                Some(update.context_id.0.clone()),
                Some(update.task_id.0.clone()),
                Some(update.status.state),
            ),
            StreamResponse::Task(task) => {
                (Some(task.context_id.0.clone()), Some(task.id.0.clone()), Some(task.status.state))
            }
            StreamResponse::Message(message) => {
                (message.context_id.as_ref().map(|c| c.0.clone()), None, None)
            }
            _ => (None, None, None),
        };
        let waiting = state.is_some_and(waits);
        if let Some(context_id) = context_id {
            event.actions.state_delta.insert(keys.context_id.clone(), context_id.into());
        }
        let task_id = task_id.filter(|_| waiting);
        event.actions.state_delta.insert(
            keys.task_id.clone(),
            task_id.map_or(serde_json::Value::Null, serde_json::Value::String),
        );
        event
    }

    /// Whether `response` is the last of its stream: a direct message, or a task or status
    /// in a state the task stops in.
    fn ends_stream(response: &a2a_protocol_types::events::StreamResponse) -> bool {
        use a2a_protocol_types::events::StreamResponse;
        match response {
            StreamResponse::StatusUpdate(update) => is_final_state(update.status.state),
            StreamResponse::Task(task) => is_final_state(task.status.state),
            StreamResponse::Message(_) => true,
            _ => false,
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
                // The wire default: let the card decide rather than forcing a mode.
                streaming: None,
                credential_broker: None,
                credential_scopes: Vec::new(),
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
                // The wire default: let the card decide rather than forcing a mode.
                streaming: None,
                credential_broker: None,
                credential_scopes: Vec::new(),
            });
            assert!(agent.sub_agents().is_empty());
        }

        #[test]
        fn build_v1_message_with_content() {
            let msg = build_v1_message(vec![a2a_protocol_types::Part::text("hello")], None, None);
            assert_eq!(msg.role, a2a_protocol_types::MessageRole::User);
            assert_eq!(msg.parts.len(), 1);
            assert_eq!(msg.parts[0].text_content(), Some("hello"));
            assert!(msg.context_id.is_none() && msg.task_id.is_none());
        }

        #[test]
        fn build_v1_message_without_content() {
            let msg = build_v1_message(Vec::new(), None, None);
            assert_eq!(msg.parts[0].text_content(), Some(""));
        }

        #[test]
        fn a_message_continues_the_remote_conversation_and_its_waiting_task() {
            let msg = build_v1_message(
                vec![a2a_protocol_types::Part::text("again")],
                Some("ctx-7".to_string()),
                Some("task-7".to_string()),
            );
            assert_eq!(msg.context_id.unwrap().0, "ctx-7");
            assert_eq!(msg.task_id.unwrap().0, "task-7");
        }

        #[test]
        fn answers_and_media_go_out_in_the_shape_every_sdk_reads() {
            let answer = outgoing_part(&Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "request_approval",
                    serde_json::json!({"status": "approved"}),
                ),
                id: Some("call-1".to_string()),
                annotations: None,
            })
            .unwrap();
            assert_eq!(
                serde_json::to_value(&answer).unwrap(),
                serde_json::json!({
                    "data": {"id": "call-1", "name": "request_approval", "response": {"status": "approved"}},
                    "metadata": {"adk_type": "function_response"}
                })
            );
            let image = outgoing_part(&Part::InlineData {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
                uri: None,
                annotations: None,
            })
            .unwrap();
            assert_eq!(image.media_type.as_deref(), Some("image/png"));
            assert!(
                outgoing_part(&Part::Thinking { thinking: "hm".into(), signature: None }).is_none()
            );
        }

        #[test]
        fn a_waiting_remote_ends_the_turn_on_its_pending_call_and_keeps_its_task() {
            use crate::a2a::v1::convert::{
                tagged_function_call_part, tagged_function_response_part,
            };
            use a2a_protocol_types::events::{StreamResponse, TaskStatusUpdateEvent};
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let mut status = TaskStatus::new(TaskState::InputRequired);
            status.message = Some(a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new("m"),
                role: a2a_protocol_types::MessageRole::Agent,
                parts: vec![
                    tagged_function_call_part(
                        "request_approval",
                        &serde_json::json!({"amount": 5}),
                        Some("call-1"),
                        true,
                    ),
                    tagged_function_response_part(
                        "request_approval",
                        &serde_json::json!({"status": "pending"}),
                        Some("call-1"),
                    ),
                ],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            });
            let resp = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                status,
                metadata: None,
            });
            let keys = RemoteKeys::for_agent("remote");
            let event = finish(&resp, &keys);

            assert!(event.llm_response.turn_complete);
            assert_eq!(event.long_running_tool_ids, vec!["call-1".to_string()]);
            let parts = &event.llm_response.content.as_ref().unwrap().parts;
            assert!(
                matches!(&parts[0], Part::FunctionCall { name, .. } if name == "request_approval")
            );
            assert_eq!(event.actions.state_delta[&keys.context_id], "ctx-1");
            assert_eq!(event.actions.state_delta[&keys.task_id], "task-1");

            // A finished remote task is not kept.
            let done = StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id: TaskId::new("task-1"),
                context_id: ContextId::new("ctx-1"),
                status: TaskStatus::new(TaskState::Completed),
                metadata: None,
            });
            let event = finish(&done, &keys);
            assert_eq!(event.actions.state_delta[&keys.context_id], "ctx-1");
            assert!(event.actions.state_delta[&keys.task_id].is_null());
        }

        /// The event a remote turn ends on, as the agent's run builds it.
        fn finish(resp: &a2a_protocol_types::events::StreamResponse, keys: &RemoteKeys) -> Event {
            let mut remote = RemoteEvents::new("inv", "remote");
            let event = remote.handle(resp).pop().unwrap_or_else(|| remote.new_event());
            end_turn(event, resp, keys)
        }

        #[test]
        fn stream_payloads_parse_bare_wrapped_and_error_forms() {
            use a2a_protocol_types::events::StreamResponse;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let update =
                StreamResponse::StatusUpdate(a2a_protocol_types::events::TaskStatusUpdateEvent {
                    task_id: TaskId::new("t1"),
                    context_id: ContextId::new("c1"),
                    status: TaskStatus::new(TaskState::InputRequired),
                    metadata: None,
                });
            let bare = serde_json::to_string(&update).unwrap();
            let wrapped =
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": update}).to_string();
            use crate::a2a::client::v1_client::{V1ClientError, parse_stream_payload};
            for data in [bare, wrapped] {
                let parsed = parse_stream_payload(&data).unwrap().unwrap();
                assert!(ends_stream(&parsed));
            }

            let error = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"boom"}}"#;
            assert!(matches!(
                parse_stream_payload(error).unwrap(),
                Err(V1ClientError::JsonRpc { code: -32603, ref message, .. }) if message == "boom"
            ));
            assert!(parse_stream_payload("not json").is_none());
        }

        #[test]
        fn working_and_artifact_updates_do_not_end_a_stream() {
            use a2a_protocol_types::events::StreamResponse;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let working =
                StreamResponse::StatusUpdate(a2a_protocol_types::events::TaskStatusUpdateEvent {
                    task_id: TaskId::new("t1"),
                    context_id: ContextId::new("c1"),
                    status: TaskStatus::new(TaskState::Working),
                    metadata: None,
                });
            assert!(!ends_stream(&working));
            let artifact = StreamResponse::ArtifactUpdate(
                a2a_protocol_types::events::TaskArtifactUpdateEvent {
                    task_id: TaskId::new("t1"),
                    context_id: ContextId::new("c1"),
                    artifact: a2a_protocol_types::artifact::Artifact::new(
                        a2a_protocol_types::ArtifactId::new("a1"),
                        vec![a2a_protocol_types::Part::text("hi")],
                    ),
                    append: None,
                    last_chunk: Some(true),
                    metadata: None,
                },
            );
            assert!(!ends_stream(&artifact));
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
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&resp).unwrap();

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
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&resp).unwrap();

            assert!(event.llm_response.turn_complete);
            assert!(event.llm_response.content.is_none());
        }

        #[test]
        fn a_failed_status_message_is_the_remote_error() {
            use a2a_protocol_types::events::TaskStatusUpdateEvent;
            use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};

            let failed = |parts: Vec<a2a_protocol_types::Part>| {
                let mut status = TaskStatus::new(TaskState::Failed);
                status.message = Some(a2a_protocol_types::Message {
                    id: a2a_protocol_types::MessageId::new("status-msg-1"),
                    role: a2a_protocol_types::MessageRole::Agent,
                    parts,
                    task_id: None,
                    context_id: None,
                    reference_task_ids: None,
                    extensions: None,
                    metadata: None,
                });
                a2a_protocol_types::events::StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                    task_id: TaskId::new("task-1"),
                    context_id: ContextId::new("ctx-1"),
                    status,
                    metadata: None,
                })
            };

            // An ADK server marks its error text: it is the error, not model output.
            let marked = failed(vec![
                crate::a2a::v1::convert::error_message_part("Internal error: agent error: boom"),
                a2a_protocol_types::Part::text("partial answer"),
            ]);
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&marked).unwrap();
            assert_eq!(
                event.llm_response.error_message.as_deref(),
                Some("Internal error: agent error: boom")
            );
            let texts: Vec<_> =
                event.content().unwrap().parts.iter().filter_map(Part::text).collect();
            assert_eq!(texts, ["partial answer"], "the rest of the message is content");
            assert!(event.llm_response.turn_complete);

            // Unmarked, the first text is still the error and stays in the content, as
            // adk-go reads it; with no message the error is generic.
            let unmarked = failed(vec![a2a_protocol_types::Part::text("boom")]);
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&unmarked).unwrap();
            assert_eq!(event.llm_response.error_message.as_deref(), Some("boom"));
            assert_eq!(event.content().unwrap().parts.len(), 1);
        }

        fn artifact_update(
            id: &str,
            text: &str,
            append: Option<bool>,
            last_chunk: Option<bool>,
            metadata: Option<serde_json::Value>,
        ) -> a2a_protocol_types::events::StreamResponse {
            a2a_protocol_types::events::StreamResponse::ArtifactUpdate(
                a2a_protocol_types::events::TaskArtifactUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new("task-1"),
                    context_id: a2a_protocol_types::ContextId::new("ctx-1"),
                    artifact: a2a_protocol_types::Artifact::new(
                        a2a_protocol_types::ArtifactId::new(id),
                        vec![a2a_protocol_types::Part::text(text)],
                    ),
                    append,
                    last_chunk,
                    metadata,
                },
            )
        }

        fn completed() -> a2a_protocol_types::events::StreamResponse {
            a2a_protocol_types::events::StreamResponse::StatusUpdate(
                a2a_protocol_types::events::TaskStatusUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new("task-1"),
                    context_id: a2a_protocol_types::ContextId::new("ctx-1"),
                    status: a2a_protocol_types::TaskStatus::new(
                        a2a_protocol_types::TaskState::Completed,
                    ),
                    metadata: None,
                },
            )
        }

        fn stored_text(events: &[Event]) -> Vec<String> {
            events
                .iter()
                .filter(|event| !event.llm_response.partial)
                .filter_map(|event| event.content())
                .map(|content| content.parts.iter().filter_map(Part::text).collect())
                .filter(|text: &String| !text.is_empty())
                .collect()
        }

        #[test]
        fn chunks_from_a_go_server_are_partial_and_join_into_one_answer() {
            // a2a-go leaves `lastChunk` out when it is false.
            let mut remote = RemoteEvents::new("inv", "remote");
            let mut events = Vec::new();
            events.extend(remote.handle(&artifact_update("a", "Hel", None, None, None)));
            events.extend(remote.handle(&artifact_update("a", "lo", Some(true), None, None)));
            events.extend(remote.handle(&completed()));

            assert!(events[0].llm_response.partial && events[1].llm_response.partial);
            assert_eq!(stored_text(&events), ["Hello"], "the session keeps the whole answer once");
        }

        #[test]
        fn a_last_chunk_closes_its_artifacts_answer() {
            let mut remote = RemoteEvents::new("inv", "remote");
            let mut events = Vec::new();
            events.extend(remote.handle(&artifact_update("a", "one ", None, Some(false), None)));
            events.extend(remote.handle(&artifact_update(
                "a",
                "two",
                Some(true),
                Some(true),
                None,
            )));
            events.extend(remote.handle(&completed()));
            assert_eq!(stored_text(&events), ["one two"]);
        }

        #[test]
        fn an_adk_servers_partial_artifact_and_its_reset_are_not_stored() {
            let partial = Some(serde_json::json!({"adk_partial": true}));
            let answer = Some(serde_json::json!({"adk_partial": false}));
            let mut remote = RemoteEvents::new("inv", "remote");
            let mut events = Vec::new();
            events.extend(remote.handle(&artifact_update(
                "tmp",
                "Hel",
                Some(false),
                None,
                partial.clone(),
            )));
            events.extend(remote.handle(&artifact_update(
                "main",
                "Hello",
                Some(false),
                None,
                answer,
            )));
            // The server empties its temporary artifact before the final status.
            let reset = a2a_protocol_types::events::StreamResponse::ArtifactUpdate(
                a2a_protocol_types::events::TaskArtifactUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new("task-1"),
                    context_id: a2a_protocol_types::ContextId::new("ctx-1"),
                    artifact: a2a_protocol_types::Artifact::new(
                        a2a_protocol_types::ArtifactId::new("tmp"),
                        vec![a2a_protocol_types::Part::data(serde_json::json!({}))],
                    ),
                    append: Some(false),
                    last_chunk: Some(true),
                    metadata: partial,
                },
            );
            assert!(remote.handle(&reset).is_empty(), "the reset is dropped");
            events.extend(remote.handle(&completed()));
            assert_eq!(stored_text(&events), ["Hello"]);
        }

        #[test]
        fn progress_before_the_task_ends_is_the_remotes_reasoning() {
            use a2a_protocol_types::task::{TaskState, TaskStatus};
            let mut status = TaskStatus::new(TaskState::Working);
            status.message = Some(a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new("m"),
                role: a2a_protocol_types::MessageRole::Agent,
                parts: vec![a2a_protocol_types::Part::text("looking it up")],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            });
            let working = a2a_protocol_types::events::StreamResponse::StatusUpdate(
                a2a_protocol_types::events::TaskStatusUpdateEvent {
                    task_id: a2a_protocol_types::TaskId::new("task-1"),
                    context_id: a2a_protocol_types::ContextId::new("ctx-1"),
                    status,
                    metadata: Some(
                        serde_json::json!({"adk_escalate": true, "adk_error_code": "E1"}),
                    ),
                },
            );
            let event = RemoteEvents::new("inv", "remote").to_event(&working).unwrap();
            assert!(event.llm_response.partial && !event.llm_response.turn_complete);
            assert!(
                matches!(&event.content().unwrap().parts[0], Part::Thinking { thinking, .. } if thinking == "looking it up")
            );
            assert!(event.actions.escalate);
            assert_eq!(event.llm_response.error_code.as_deref(), Some("E1"));
            assert_eq!(event.provider_metadata[A2A_TASK_ID_KEY], "task-1");
            assert_eq!(event.provider_metadata[A2A_CONTEXT_ID_KEY], "ctx-1");
        }

        #[test]
        fn a_direct_message_reply_ends_the_turn_with_its_text() {
            let reply = a2a_protocol_types::Message {
                id: a2a_protocol_types::MessageId::new("m-1"),
                role: a2a_protocol_types::MessageRole::Agent,
                parts: vec![a2a_protocol_types::Part::text("hello there")],
                task_id: None,
                context_id: None,
                reference_task_ids: None,
                extensions: None,
                metadata: None,
            };
            let resp = a2a_protocol_types::events::StreamResponse::Message(reply);

            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&resp).unwrap();
            assert_eq!(event.author, "agent-1");
            assert!(event.llm_response.turn_complete);
            let text: String = event
                .llm_response
                .content
                .expect("content")
                .parts
                .iter()
                .filter_map(Part::text)
                .collect();
            assert_eq!(text, "hello there");
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
            let result = RemoteEvents::new("inv-1", "agent-1").to_event(&resp);

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
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&resp).unwrap();

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
            let event = RemoteEvents::new("inv-1", "agent-1").to_event(&resp).unwrap();

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
