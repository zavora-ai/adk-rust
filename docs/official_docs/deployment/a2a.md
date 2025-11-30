# Agent-to-Agent (A2A) Protocol

The Agent-to-Agent (A2A) Protocol enables agents to communicate and collaborate across network boundaries. ADK-Rust provides full support for both exposing agents via A2A and consuming remote A2A agents.

## Overview

A2A is useful when:
- Integrating with third-party agent services
- Building microservices architectures with specialized agents
- Enabling cross-language agent communication
- Enforcing formal contracts between agent systems

For simple internal organization, use local sub-agents instead of A2A for better performance.

## Agent Cards

Every A2A agent exposes an agent card that describes its capabilities. The card is automatically generated and served at `/.well-known/agent.json`.

```rust
use adk_server::a2a::build_agent_card;

let agent_card = build_agent_card(&agent, "http://localhost:8080");

println!("Agent: {}", agent_card.name);
println!("Skills: {}", agent_card.skills.len());
println!("Streaming: {}", agent_card.capabilities.streaming);
```

The agent card includes:
- Agent name and description
- Base URL for communication
- Capabilities (streaming, state history, etc.)
- Skills derived from the agent and its sub-agents

## Exposing an Agent via A2A

To expose an agent so other agents can use it, create an HTTP server with A2A endpoints:

```rust
use adk_server::{create_app_with_a2a, ServerConfig};
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create your agent
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("Answers weather questions")
        .model(Arc::new(model))
        .build()?;

    // Create server config
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::new(agent))),
        Arc::new(InMemorySessionService::new()),
    );

    // Create app with A2A support
    let app = create_app_with_a2a(config, Some("http://localhost:8080"));

    // Serve
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

This exposes:
- `GET /.well-known/agent.json` - Agent card
- `POST /a2a` - JSON-RPC endpoint for A2A protocol
- `POST /a2a/stream` - SSE streaming endpoint

## Consuming a Remote Agent

Use `RemoteA2aAgent` to communicate with a remote A2A agent:

```rust
use adk_server::a2a::RemoteA2aAgent;

let remote_agent = RemoteA2aAgent::builder("prime_checker")
    .description("Checks if numbers are prime")
    .agent_url("http://localhost:8001")
    .build()?;

// Use as a sub-agent
let root_agent = LlmAgentBuilder::new("root")
    .model(Arc::new(model))
    .sub_agent(Arc::new(remote_agent))
    .build()?;
```

The `RemoteA2aAgent`:
- Automatically fetches the agent card from the remote URL
- Converts ADK events to/from A2A protocol messages
- Handles streaming responses
- Works seamlessly as a sub-agent

## A2A Client

For direct communication with remote agents, use the `A2aClient`:

```rust
use adk_server::a2a::{A2aClient, Message, Part, Role};

// Create client from URL (fetches agent card)
let client = A2aClient::from_url("http://localhost:8080").await?;

// Build a message
let message = Message::builder()
    .role(Role::User)
    .parts(vec![Part::text("What's the weather?".to_string())])
    .message_id(uuid::Uuid::new_v4().to_string())
    .build();

// Send message (blocking)
let response = client.send_message(message.clone()).await?;

// Or send with streaming
let mut stream = client.send_streaming_message(message).await?;
while let Some(event) = stream.next().await {
    match event? {
        UpdateEvent::TaskArtifactUpdate(artifact) => {
            println!("Artifact: {:?}", artifact);
        }
        UpdateEvent::TaskStatusUpdate(status) => {
            println!("Status: {:?}", status.status.state);
        }
    }
}
```

## JSON-RPC Protocol

ADK-Rust implements the A2A protocol using JSON-RPC 2.0. Supported methods:

### message/send

Send a message to the agent:

```json
{
  "jsonrpc": "2.0",
  "method": "message/send",
  "params": {
    "message": {
      "role": "user",
      "messageId": "msg-123",
      "parts": [{"text": "Hello!"}]
    }
  },
  "id": 1
}
```

Response includes a task object with status and artifacts.

### message/stream

Same as `message/send` but returns Server-Sent Events (SSE) for streaming responses.

### tasks/cancel

Cancel a running task:

```json
{
  "jsonrpc": "2.0",
  "method": "tasks/cancel",
  "params": {
    "taskId": "task-123"
  },
  "id": 1
}
```

## Multi-Agent Example

Combine local and remote agents:

```rust
// Local agent
let roll_agent = LlmAgentBuilder::new("roll_agent")
    .description("Rolls dice")
    .model(Arc::new(model.clone()))
    .tool(Arc::new(roll_die_tool))
    .build()?;

// Remote agent
let prime_agent = RemoteA2aAgent::builder("prime_agent")
    .description("Checks if numbers are prime")
    .agent_url("http://localhost:8001")
    .build()?;

// Root agent orchestrates both
let root_agent = LlmAgentBuilder::new("root_agent")
    .instruction("Delegate dice rolling to roll_agent and prime checking to prime_agent")
    .model(Arc::new(model))
    .sub_agent(Arc::new(roll_agent))
    .sub_agent(Arc::new(prime_agent))
    .build()?;
```

## Error Handling

A2A operations return standard ADK errors:

```rust
match client.send_message(message).await {
    Ok(response) => {
        if let Some(error) = response.error {
            eprintln!("RPC error: {} (code: {})", error.message, error.code);
        }
    }
    Err(e) => {
        eprintln!("Request failed: {}", e);
    }
}
```

Common error codes:
- `-32600`: Invalid request
- `-32601`: Method not found
- `-32602`: Invalid params
- `-32603`: Internal error

## Best Practices

1. **Use agent cards**: Always fetch and validate agent cards before communication
2. **Handle streaming**: Use streaming for long-running operations
3. **Error recovery**: Implement retry logic for network failures
4. **Timeouts**: Set appropriate timeouts for remote calls
5. **Security**: Use HTTPS in production and implement authentication

## Security Configuration

Configure CORS, timeouts, and security headers for production deployments:

```rust
use adk_server::{ServerConfig, SecurityConfig};
use std::time::Duration;

// Production configuration
let config = ServerConfig::new(agent_loader, session_service)
    .with_allowed_origins(vec![
        "https://myapp.com".to_string(),
        "https://admin.myapp.com".to_string(),
    ])
    .with_request_timeout(Duration::from_secs(30))
    .with_max_body_size(10 * 1024 * 1024);  // 10MB

// Or use presets
let dev_config = ServerConfig::new(agent_loader, session_service)
    .with_security(SecurityConfig::development());  // Permissive for dev

let prod_config = ServerConfig::new(agent_loader, session_service)
    .with_security(SecurityConfig::production(allowed_origins));
```

Security features include:
- **CORS**: Configurable allowed origins (default: permissive for development)
- **Request timeout**: Configurable timeout (default: 30 seconds)
- **Body size limit**: Maximum request body size (default: 10MB)
- **Security headers**: X-Content-Type-Options, X-Frame-Options, X-XSS-Protection
- **Error sanitization**: Internal errors are logged but not exposed to clients in production

## Related

- [LlmAgent](../agents/llm-agent.md) - Creating agents
- [Multi-Agent Systems](../agents/multi-agent.md) - Sub-agents and hierarchies
- [Server Deployment](server.md) - Running agents as HTTP servers
