# Phase 7: Server & API - Go Implementation Analysis

## Overview
Analysis of how Google ADK Go implements the REST API and A2A protocol server.

## Architecture

### Entry Point: `server/adkrest/handler.go`
```go
func NewHandler(config *launcher.Config) http.Handler
```
- Takes a `launcher.Config` with:
  - `SessionService` - session management
  - `AgentLoader` - loads agents by app name
  - `ArtifactService` - artifact storage
- Returns standard `http.Handler` (framework-agnostic)
- Uses Gorilla Mux for routing
- Sets up telemetry with OpenTelemetry

### Controllers Structure

#### 1. **SessionsAPIController** (`controllers/sessions.go`)
**Dependencies**: `session.Service`

**Endpoints**:
- `CreateSessionHandler` - POST /sessions/{app_name}/{user_id}/{session_id}
- `GetSessionHandler` - GET /sessions/{app_name}/{user_id}/{session_id}
- `DeleteSessionHandler` - DELETE /sessions/{app_name}/{user_id}/{session_id}
- `ListSessionsHandler` - GET /sessions/{app_name}/{user_id}

**Key Features**:
- Extracts session ID from URL path parameters
- Handles optional request body (state, events)
- Returns JSON responses
- Uses internal models for request/response transformation

#### 2. **RuntimeAPIController** (`controllers/runtime.go`)
**Dependencies**: `session.Service`, `agent.Loader`, `artifact.Service`

**Endpoints**:
- `RunHandler` - POST /run (non-streaming)
- `RunSSEHandler` - POST /run/sse (Server-Sent Events streaming)

**Key Features**:
- Validates session exists before running
- Creates `runner.Runner` on-demand per request
- Supports both streaming and non-streaming modes
- SSE implementation:
  - Sets headers: `Content-Type: text/event-stream`, `Cache-Control: no-cache`
  - Writes `data: {json}\n` format
  - Flushes after each event
- Request body: `RunAgentRequest` with app_name, user_id, session_id, new_message, streaming flag

#### 3. **ArtifactsAPIController** (`controllers/artifacts.go`)
**Dependencies**: `artifact.Service`

**Endpoints**:
- `ListArtifactsHandler` - GET /artifacts/{app_name}/{user_id}/{session_id}
- `LoadArtifactHandler` - GET /artifacts/{app_name}/{user_id}/{session_id}/{artifact_name}
- `LoadArtifactVersionHandler` - GET /artifacts/{app_name}/{user_id}/{session_id}/{artifact_name}/{version}
- `DeleteArtifactHandler` - DELETE /artifacts/{app_name}/{user_id}/{session_id}/{artifact_name}

**Key Features**:
- Version parameter optional (query param or path param)
- Returns `Part` directly (InlineData or FileData)
- List returns array of filenames

#### 4. **AppsAPIController** (`controllers/apps.go`)
**Dependencies**: `agent.Loader`

**Endpoints**:
- Lists available apps/agents

#### 5. **DebugAPIController** (`controllers/debug.go`)
**Dependencies**: `session.Service`, `agent.Loader`, telemetry exporter

**Endpoints**:
- Debug information and telemetry

### A2A Protocol (`server/adka2a/`)

**Key Files**:
- `agent_card.go` - Generates A2A agent cards from ADK agents
- `events.go` - Converts ADK events to A2A events
- `executor.go` - Executes A2A requests
- `processor.go` - Processes A2A messages
- `parts.go` - Converts between ADK and A2A parts

**Key Functions**:
```go
func BuildAgentSkills(agent agent.Agent) []a2a.AgentSkill
```
- Extracts skills from LLM agents (model + tools)
- Extracts skills from workflow agents
- Recursively processes sub-agents
- Tags skills appropriately (llm, tools, workflow, etc.)

## Key Design Patterns

### 1. **Controller Pattern**
- Each controller is a struct with service dependencies
- Constructor function: `NewXXXController(deps...)`
- Handler methods: `func (c *Controller) HandlerName(rw http.ResponseWriter, req *http.Request)`

### 2. **On-Demand Runner Creation**
```go
r, err := runner.New(runner.Config{
    AppName:         req.AppName,
    Agent:           curAgent,
    SessionService:  c.sessionService,
    ArtifactService: c.artifactService,
})
```
- Runner is NOT a singleton
- Created per request with specific agent
- Allows different agents per app

### 3. **Model Transformation**
- Internal models in `internal/models/`
- Separate from public API types
- Conversion functions: `FromSession()`, `ToSessionEvent()`, etc.

### 4. **Error Handling**
```go
type statusError struct {
    error
    statusCode int
}
```
- Custom error type with HTTP status code
- Consistent error responses

### 5. **SSE Streaming**
```go
rw.Header().Set("Content-Type", "text/event-stream")
rw.Header().Set("Cache-Control", "no-cache")
rw.Header().Set("Connection", "keep-alive")

for event, err := range resp {
    fmt.Fprintf(rw, "data: ")
    json.NewEncoder(rw).Encode(event)
    fmt.Fprintf(rw, "\n")
    flusher.Flush()
}
```

## Configuration Structure

### `launcher.Config`
```go
type Config struct {
    AgentLoader     agent.Loader
    SessionService  session.Service
    ArtifactService artifact.Service
    MemoryService   memory.Service  // optional
}
```

### `agent.Loader`
```go
type Loader interface {
    LoadAgent(appName string) (Agent, error)
}

// SingleLoader - loads same agent for all apps
func NewSingleLoader(agent Agent) Loader
```

## REST API Routes (from routers/)

### Sessions
- POST   `/sessions/{app_name}/{user_id}/{session_id}`
- GET    `/sessions/{app_name}/{user_id}/{session_id}`
- DELETE `/sessions/{app_name}/{user_id}/{session_id}`
- GET    `/sessions/{app_name}/{user_id}`

### Runtime
- POST `/run` - non-streaming execution
- POST `/run/sse` - streaming execution with SSE

### Artifacts
- GET    `/artifacts/{app_name}/{user_id}/{session_id}`
- GET    `/artifacts/{app_name}/{user_id}/{session_id}/{artifact_name}[?version=N]`
- DELETE `/artifacts/{app_name}/{user_id}/{session_id}/{artifact_name}`

### Apps
- GET `/apps` - list available apps

### Debug
- GET `/debug/...` - various debug endpoints

## Implementation Notes for Rust

### What We Need:

1. **Server Configuration Struct**
   ```rust
   pub struct ServerConfig {
       pub agent_loader: Arc<dyn AgentLoader>,
       pub session_service: Arc<dyn SessionService>,
       pub artifact_service: Arc<dyn ArtifactService>,
       pub memory_service: Option<Arc<dyn MemoryService>>,
   }
   ```

2. **AgentLoader Trait**
   ```rust
   #[async_trait]
   pub trait AgentLoader: Send + Sync {
       async fn load_agent(&self, app_name: &str) -> Result<Arc<dyn Agent>>;
   }
   
   // SingleAgentLoader implementation
   pub struct SingleAgentLoader {
       agent: Arc<dyn Agent>,
   }
   ```

3. **Controllers with State**
   ```rust
   #[derive(Clone)]
   pub struct RuntimeController {
       session_service: Arc<dyn SessionService>,
       agent_loader: Arc<dyn AgentLoader>,
       artifact_service: Arc<dyn ArtifactService>,
   }
   ```

4. **SSE Streaming with Axum**
   ```rust
   use axum::response::sse::{Event, Sse};
   use futures::stream::Stream;
   
   pub async fn run_sse(
       State(controller): State<RuntimeController>,
       Json(req): Json<RunRequest>,
   ) -> Sse<impl Stream<Item = Result<Event, Infallible>>>
   ```

5. **On-Demand Runner Creation**
   ```rust
   let agent = controller.agent_loader.load_agent(&req.app_name).await?;
   let runner = Runner::new(RunnerConfig {
       app_name: req.app_name.clone(),
       agent,
       session_service: controller.session_service.clone(),
       artifact_service: controller.artifact_service.clone(),
   })?;
   ```

### Key Differences from Current Implementation:

1. **No Singleton Runner** - Create runner per request
2. **AgentLoader Pattern** - Load agents dynamically by app name
3. **Proper SSE** - Use Axum's SSE support, not manual implementation
4. **Controller State** - Each controller holds service references
5. **Model Separation** - Internal models vs API models

### Phase 7 Tasks Breakdown:

✅ **Task 7.1: REST API Foundation** - COMPLETE
- Axum setup
- Basic routing
- Health check
- CORS middleware

✅ **Task 7.2: Session Endpoints** - COMPLETE  
- CRUD operations
- Tests passing

❌ **Task 7.3: Runtime Endpoints** - TODO
- Need AgentLoader trait
- Need on-demand Runner creation
- Need SSE streaming support
- Need RunRequest/RunResponse models

❌ **Task 7.4: Artifact Endpoints** - TODO
- List, Load, Delete operations
- Version support
- Part serialization

❌ **Task 7.5: A2A Protocol** - TODO
- Agent card generation
- Skill extraction
- Event conversion
- A2A executor

## Next Steps

1. Create `AgentLoader` trait in adk-core or adk-agent
2. Implement `SingleAgentLoader`
3. Create `ServerConfig` struct
4. Implement Runtime controller with SSE
5. Implement Artifact controller
6. Implement A2A protocol types and conversion
