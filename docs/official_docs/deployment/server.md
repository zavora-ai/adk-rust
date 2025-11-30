# Server API

The ADK-Rust server provides a REST API for running agents, managing sessions, and accessing artifacts. When you deploy your agent using the [Launcher](launcher.md) in server mode, it exposes these endpoints along with a web UI.

## Overview

The server is built on Axum and provides:

- **REST API**: HTTP endpoints for agent execution and session management
- **Server-Sent Events (SSE)**: Real-time streaming of agent responses
- **Web UI**: Interactive browser-based interface
- **CORS Support**: Cross-origin requests enabled
- **Telemetry**: Built-in observability with tracing

## Starting the Server

Use the Launcher to start the server:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);
    
    let agent = LlmAgentBuilder::new("my_agent")
        .description("A helpful assistant")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;
    
    Launcher::new(Arc::new(agent)).run().await
}
```

Start the server:

```bash
cargo run -- serve --port 8080
```

## REST API Endpoints

### Health Check

Check if the server is running:

```
GET /api/health
```

**Response:**
```
OK
```

### Run Agent with Streaming

Execute an agent and stream responses using Server-Sent Events:

```
POST /api/run_sse
```

**Request Body:**
```json
{
  "appName": "my_agent",
  "userId": "user123",
  "sessionId": "session456",
  "newMessage": {
    "role": "user",
    "parts": [
      {
        "text": "What is the capital of France?"
      }
    ]
  },
  "streaming": true
}
```

**Response:**
- Content-Type: `text/event-stream`
- Streams events as JSON objects

**Event Format:**
```json
{
  "id": "evt_123",
  "timestamp": 1234567890,
  "author": "my_agent",
  "content": {
    "role": "model",
    "parts": [
      {
        "text": "The capital of France is Paris."
      }
    ]
  },
  "actions": {},
  "llm_response": {
    "content": {
      "role": "model",
      "parts": [
        {
          "text": "The capital of France is Paris."
        }
      ]
    }
  }
}
```

### Session Management

#### Create Session

Create a new session:

```
POST /api/sessions
```

**Request Body:**
```json
{
  "appName": "my_agent",
  "userId": "user123",
  "sessionId": "session456"
}
```

**Response:**
```json
{
  "id": "session456",
  "appName": "my_agent",
  "userId": "user123",
  "lastUpdateTime": 1234567890,
  "events": [],
  "state": {}
}
```

#### Get Session

Retrieve session details:

```
GET /api/sessions/:app_name/:user_id/:session_id
```

**Response:**
```json
{
  "id": "session456",
  "appName": "my_agent",
  "userId": "user123",
  "lastUpdateTime": 1234567890,
  "events": [],
  "state": {}
}
```

#### Delete Session

Delete a session:

```
DELETE /api/sessions/:app_name/:user_id/:session_id
```

**Response:**
- Status: `204 No Content`

#### List Sessions

List all sessions for a user:

```
GET /api/apps/:app_name/users/:user_id/sessions
```

**Response:**
```json
[
  {
    "id": "session456",
    "appName": "my_agent",
    "userId": "user123",
    "lastUpdateTime": 1234567890,
    "events": [],
    "state": {}
  }
]
```

### Artifact Management

#### List Artifacts

List all artifacts for a session:

```
GET /api/sessions/:app_name/:user_id/:session_id/artifacts
```

**Response:**
```json
[
  "image1.png",
  "document.pdf",
  "data.json"
]
```

#### Get Artifact

Download an artifact:

```
GET /api/sessions/:app_name/:user_id/:session_id/artifacts/:artifact_name
```

**Response:**
- Content-Type: Determined by file extension
- Body: Binary or text content

### Application Management

#### List Applications

List all available agents:

```
GET /api/apps
GET /api/list-apps  (legacy compatibility)
```

**Response:**
```json
{
  "apps": [
    {
      "name": "my_agent",
      "description": "A helpful assistant"
    }
  ]
}
```

## Web UI

The server includes a built-in web UI accessible at:

```
http://localhost:8080/ui/
```

### Features

- **Interactive Chat**: Send messages and receive streaming responses
- **Session Management**: Create, view, and switch between sessions
- **Multi-Agent Support**: Visualize agent transfers and hierarchies
- **Artifact Viewer**: View and download session artifacts
- **Real-time Updates**: SSE-based streaming for instant responses

### UI Routes

- `/` - Redirects to `/ui/`
- `/ui/` - Main chat interface
- `/ui/assets/*` - Static assets (CSS, JS, images)
- `/ui/assets/config/runtime-config.json` - Runtime configuration

## Client Examples

### JavaScript/TypeScript

Using the Fetch API with SSE:

```javascript
async function runAgent(message) {
  const response = await fetch('http://localhost:8080/api/run_sse', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      appName: 'my_agent',
      userId: 'user123',
      sessionId: 'session456',
      newMessage: {
        role: 'user',
        parts: [{ text: message }]
      },
      streaming: true
    })
  });

  const reader = response.body.getReader();
  const decoder = new TextDecoder();

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    
    const chunk = decoder.decode(value);
    const lines = chunk.split('\n');
    
    for (const line of lines) {
      if (line.startsWith('data: ')) {
        const event = JSON.parse(line.slice(6));
        console.log('Event:', event);
      }
    }
  }
}
```

### Python

Using the `requests` library:

```python
import requests
import json

def run_agent(message):
    url = 'http://localhost:8080/api/run_sse'
    payload = {
        'appName': 'my_agent',
        'userId': 'user123',
        'sessionId': 'session456',
        'newMessage': {
            'role': 'user',
            'parts': [{'text': message}]
        },
        'streaming': True
    }
    
    response = requests.post(url, json=payload, stream=True)
    
    for line in response.iter_lines():
        if line:
            line_str = line.decode('utf-8')
            if line_str.startswith('data: '):
                event = json.loads(line_str[6:])
                print('Event:', event)

run_agent('What is the capital of France?')
```

### cURL

```bash
# Create session
curl -X POST http://localhost:8080/api/sessions \
  -H "Content-Type: application/json" \
  -d '{
    "appName": "my_agent",
    "userId": "user123",
    "sessionId": "session456"
  }'

# Run agent with streaming
curl -X POST http://localhost:8080/api/run_sse \
  -H "Content-Type: application/json" \
  -d '{
    "appName": "my_agent",
    "userId": "user123",
    "sessionId": "session456",
    "newMessage": {
      "role": "user",
      "parts": [{"text": "What is the capital of France?"}]
    },
    "streaming": true
  }'
```

## Server Configuration

### Custom Port

Specify a custom port:

```bash
cargo run -- serve --port 3000
```

### Custom Artifact Service

Provide your own artifact service:

```rust
use adk_artifact::InMemoryArtifactService;

let artifact_service = Arc::new(InMemoryArtifactService::new());

Launcher::new(Arc::new(agent))
    .with_artifact_service(artifact_service)
    .run()
    .await
```

### Custom Session Service

For production deployments, use a persistent session service:

```rust
use adk_session::DatabaseSessionService;

// Note: This requires implementing a custom server setup
// The Launcher uses InMemorySessionService by default
```

## Error Handling

The API uses standard HTTP status codes:

| Status Code | Meaning |
|-------------|---------|
| 200 | Success |
| 204 | Success (No Content) |
| 400 | Bad Request |
| 404 | Not Found |
| 500 | Internal Server Error |

**Error Response Format:**
```json
{
  "error": "Error message description"
}
```

## CORS Configuration

The server enables permissive CORS by default, allowing requests from any origin. This is suitable for development but should be restricted in production.

## Telemetry

The server automatically initializes telemetry when started. Logs are output to stdout with structured formatting.

**Log Levels:**
- `ERROR`: Critical errors
- `WARN`: Warnings
- `INFO`: General information (default)
- `DEBUG`: Detailed debugging
- `TRACE`: Very detailed tracing

Set the log level with the `RUST_LOG` environment variable:

```bash
RUST_LOG=debug cargo run -- serve
```

## Best Practices

1. **Session Management**: Always create a session before running an agent
2. **Error Handling**: Check HTTP status codes and handle errors appropriately
3. **Streaming**: Use SSE for real-time responses; parse events line by line
4. **Security**: In production, implement authentication and restrict CORS
5. **Persistence**: Use `DatabaseSessionService` for production deployments
6. **Monitoring**: Enable telemetry and monitor logs for issues

## Full-Stack Example

For a complete working example of a frontend application interacting with an ADK backend, see the **Research Paper Generator** example. This demonstrates:

- **Frontend**: HTML/JavaScript client with real-time streaming
- **Backend**: ADK agent with custom research and PDF generation tools
- **Integration**: Complete REST API usage with SSE streaming
- **Artifacts**: PDF generation and download
- **Session Management**: Automatic session creation and handling

The example shows a production-ready pattern for building AI-powered web applications with ADK-Rust.

**Quick Start:**
```bash
# Start the server
cargo run --example full_stack_research -p adk-rust-guide -- serve --port 8080

# Open the frontend
open examples/research_paper/frontend.html
```

**Files:**
- Backend: `adk-rust-guide/examples/deployment/full_stack_research.rs`
- Frontend: `examples/research_paper/frontend.html`
- Documentation: `examples/research_paper/README.md`
- Architecture: `examples/research_paper/architecture.md`

## Related

- [Launcher](launcher.md) - Starting the server
- [Sessions](../sessions/sessions.md) - Session management
- [Artifacts](../artifacts/artifacts.md) - Artifact storage
- [Observability](../observability/telemetry.md) - Telemetry and logging
