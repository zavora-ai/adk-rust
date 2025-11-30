use crate::a2a::{
    AgentCard, JsonRpcRequest, JsonRpcResponse, Message, MessageSendParams,
    TaskStatusUpdateEvent, TaskArtifactUpdateEvent, UpdateEvent,
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
        Self {
            http_client: reqwest::Client::new(),
            agent_card,
        }
    }

    /// Resolve an agent card from a URL (fetch from /.well-known/agent.json)
    pub async fn resolve_agent_card(base_url: &str) -> Result<AgentCard> {
        let url = format!(
            "{}/.well-known/agent.json",
            base_url.trim_end_matches('/')
        );

        let client = reqwest::Client::new();
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| adk_core::AdkError::Agent(format!("Failed to fetch agent card: {}", e)))?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::Agent(format!(
                "Failed to fetch agent card: HTTP {}",
                response.status()
            )));
        }

        let card: AgentCard = response
            .json()
            .await
            .map_err(|e| adk_core::AdkError::Agent(format!("Failed to parse agent card: {}", e)))?;

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
            params: Some(serde_json::to_value(MessageSendParams {
                message,
                config: None,
            }).map_err(|e| adk_core::AdkError::Agent(e.to_string()))?),
            id: Some(Value::String(uuid::Uuid::new_v4().to_string())),
        };

        let response = self
            .http_client
            .post(&self.agent_card.url)
            .json(&request)
            .send()
            .await
            .map_err(|e| adk_core::AdkError::Agent(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::Agent(format!(
                "Request failed: HTTP {}",
                response.status()
            )));
        }

        let rpc_response: JsonRpcResponse = response
            .json()
            .await
            .map_err(|e| adk_core::AdkError::Agent(format!("Failed to parse response: {}", e)))?;

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
            params: Some(serde_json::to_value(MessageSendParams {
                message,
                config: None,
            }).map_err(|e| adk_core::AdkError::Agent(e.to_string()))?),
            id: Some(Value::String(uuid::Uuid::new_v4().to_string())),
        };

        let response = self
            .http_client
            .post(&stream_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| adk_core::AdkError::Agent(format!("Request failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(adk_core::AdkError::Agent(format!(
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
                        yield Err(adk_core::AdkError::Agent(format!("Stream error: {}", e)));
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
                                    yield Err(adk_core::AdkError::Agent(format!(
                                        "RPC error: {} ({})",
                                        error.message, error.code
                                    )));
                                }
                            }
                            Err(e) => {
                                // Skip parse errors for non-JSON data
                                tracing::debug!("Failed to parse SSE data: {}", e);
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
