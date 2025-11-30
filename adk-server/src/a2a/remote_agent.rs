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
        Self {
            name: name.into(),
            description: String::new(),
            agent_url: None,
        }
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
        let agent_url = self.agent_url.ok_or_else(|| {
            adk_core::AdkError::Agent("RemoteA2aAgent requires agent_url".to_string())
        })?;

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
            event.llm_response.content = Some(Content {
                role: "model".to_string(),
                parts,
            });
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
