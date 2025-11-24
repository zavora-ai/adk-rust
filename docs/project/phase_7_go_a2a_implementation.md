# Go ADK A2A Implementation - Complete Analysis

## Package: `google.golang.org/adk/server/adka2a`

**Purpose**: Expose ADK agents via the A2A (Agent-to-Agent) protocol

## Files Overview

| File | Lines | Purpose |
|------|-------|---------|
| `agent_card.go` | 9,544 | Generate A2A AgentCard from ADK agents |
| `events.go` | 8,194 | Convert between ADK Events and A2A Messages |
| `parts.go` | 7,106 | Convert between ADK Parts and A2A Parts |
| `executor.go` | 5,907 | Execute ADK agents in response to A2A requests |
| `processor.go` | 5,281 | Process ADK events into A2A events |
| `metadata.go` | 2,937 | Handle metadata conversion and tracking |

**Total**: ~39,000 lines of implementation + tests

## Core Components

### 1. AgentCard Generation (`agent_card.go`)

**Main Function**:
```go
func BuildAgentSkills(agent agent.Agent) []a2a.AgentSkill
```

**What it does**:
- Extracts skills from LLM agents (model + tools)
- Extracts skills from workflow agents (sequential, parallel, loop)
- Recursively processes sub-agents
- Generates human-readable descriptions
- Tags skills appropriately

**Skill Types Generated**:
1. **LLM Agent Skills**:
   - Primary "model" skill with instructions
   - One skill per tool
   - Tags: `["llm"]`, `["llm", "tools"]`

2. **Workflow Agent Skills**:
   - Primary orchestration skill
   - Sub-agent orchestration skill
   - Tags: `["sequential"]`, `["parallel"]`, `["loop"]`, `["orchestration"]`

3. **Sub-Agent Skills** (recursive):
   - Prefixed with parent agent name
   - Inherits parent tags + adds `sub_agent:name` tag

**Example Output**:
```go
// For LLM agent with Google Search tool:
[]a2a.AgentSkill{
    {
        ID: "weather_agent",
        Name: "model",
        Description: "Answers weather questions using Google Search",
        Tags: []string{"llm"},
    },
    {
        ID: "weather_agent-google_search",
        Name: "google_search",
        Description: "Search Google for information",
        Tags: []string{"llm", "tools"},
    },
}
```

### 2. Event Conversion (`events.go`)

**Key Functions**:

#### ADK → A2A:
```go
func EventToMessage(event *session.Event) (*a2a.Message, error)
```
- Converts ADK session events to A2A messages
- Determines role (user vs agent) from event author
- Converts parts using `ToA2AParts()`
- Preserves metadata (escalate, transfer_to_agent)

#### A2A → ADK:
```go
func ToSessionEvent(ctx agent.InvocationContext, event a2a.Event) (*session.Event, error)
```
- Handles A2A Task, Message, and ArtifactUpdateEvent
- Creates ADK events with proper invocation context
- Extracts task_id and context_id from metadata

**Metadata Keys**:
- `adk_task_id` - Maps to ADK custom metadata
- `adk_context_id` - Maps to ADK custom metadata
- `adk_escalate` - Boolean flag for escalation
- `adk_transfer_to_agent` - Target agent name

### 3. Part Conversion (`parts.go`)

**Main Function**:
```go
func ToA2AParts(parts []*genai.Part, longRunningToolIDs []string) ([]a2a.Part, error)
```

**Conversions**:

| ADK Part Type | A2A Part Type | Notes |
|---------------|---------------|-------|
| `genai.Text` | `a2a.TextPart` | Direct text conversion |
| `genai.Blob` | `a2a.FilePart` | Base64 encode data |
| `genai.FileData` | `a2a.FilePart` | Use URI reference |
| `genai.FunctionCall` | `a2a.DataPart` | JSON with function details |
| `genai.FunctionResponse` | `a2a.DataPart` | JSON with response |

**Special Handling**:
- Long-running tools marked with `"long_running": true` in metadata
- Function calls include name, args, and ID
- Function responses include name, response, and ID

**Example**:
```go
// ADK FunctionCall
&genai.Part{
    FunctionCall: &genai.FunctionCall{
        Name: "google_search",
        Args: map[string]any{"query": "weather"},
        ID: "call-123",
    },
}

// Becomes A2A DataPart
a2a.DataPart{
    Data: map[string]any{
        "function_call": map[string]any{
            "name": "google_search",
            "args": map[string]any{"query": "weather"},
            "id": "call-123",
        },
    },
    Metadata: map[string]any{
        "long_running": false,
    },
}
```

### 4. Executor (`executor.go`)

**Main Type**:
```go
type Executor struct {
    config ExecutorConfig
}

type ExecutorConfig struct {
    RunnerConfig runner.Config  // For runner.New()
    RunConfig agent.RunConfig   // For runner.Run()
}
```

**Implements**: `a2asrv.AgentExecutor` interface

**Execution Flow**:
1. **Receive A2A request** with Message
2. **Convert message** to GenAI Content
3. **Create Runner** with RunnerConfig
4. **Check/Create session** if needed
5. **Send TaskStateSubmitted** event (if new task)
6. **Send TaskStateWorking** event
7. **Stream ADK events** → convert to A2A artifact updates
8. **Send terminal event**:
   - `TaskStateFailed` if error
   - `TaskStateInputRequired` if long-running tool
   - `TaskStateCompleted` otherwise

**Event Streaming**:
```go
for event, err := range r.Run(ctx, userID, sessionID, content, runConfig) {
    // Convert each ADK event to A2A artifact update
    a2aEvent := processor.process(ctx, event)
    queue.Write(ctx, a2aEvent)
}
```

### 5. Event Processor (`processor.go`)

**Main Type**:
```go
type eventProcessor struct {
    reqCtx *a2asrv.RequestContext
    meta invocationMeta
    terminalActions session.EventActions
    responseID a2a.ArtifactID
    terminalEvents map[a2a.TaskState]*a2a.TaskStatusUpdateEvent
}
```

**Processing Logic**:

1. **For each ADK event**:
   - Extract LLM response
   - Check for errors → queue `TaskStateFailed`
   - Check for long-running tools → queue `TaskStateInputRequired`
   - Convert parts to A2A
   - Create `TaskArtifactUpdateEvent`

2. **Terminal events** (sent after all events):
   - Final artifact update with `LastChunk: true`
   - Highest priority status update:
     - Failed > InputRequired > Completed
   - Include escalate/transfer metadata

**Artifact Streaming**:
```go
// First event creates artifact
result = a2a.NewArtifactEvent(reqCtx, parts...)
responseID = result.Artifact.ID

// Subsequent events append to artifact
result = a2a.NewArtifactUpdateEvent(reqCtx, responseID, parts...)

// Final event marks completion
result = a2a.NewArtifactUpdateEvent(reqCtx, responseID)
result.LastChunk = true
```

### 6. Metadata Management (`metadata.go`)

**Invocation Metadata**:
```go
type invocationMeta struct {
    userID    string  // From A2A context or "A2A_USER_" + contextID
    sessionID string  // From A2A contextID
    eventMeta map[string]any  // Metadata for all events
}
```

**Metadata Keys** (all prefixed with `adk_`):
- `app_name` - ADK application name
- `user_id` - User identifier
- `session_id` - Session identifier
- `invocation_id` - Event invocation ID
- `author` - Event author
- `branch` - Event branch
- `error_code` - LLM error code
- `grounding_metadata` - LLM grounding info
- `escalate` - Escalation flag
- `transfer_to_agent` - Transfer target

## Integration Points

### With ADK Runner:
```go
r, err := runner.New(config.RunnerConfig)
events := r.Run(ctx, userID, sessionID, content, runConfig)
```

### With A2A Server:
```go
executor := adka2a.NewExecutor(adka2a.ExecutorConfig{
    RunnerConfig: runner.Config{
        AppName: "my-app",
        Agent: myAgent,
        SessionService: sessionSvc,
    },
    RunConfig: agent.RunConfig{
        StreamingMode: agent.StreamingModeSSE,
    },
})

// A2A server uses executor to handle requests
server := a2asrv.NewServer(executor, ...)
```

## What's NOT Implemented

1. **AgentCard HTTP Endpoint** - No REST endpoint to serve AgentCard
2. **Full A2A Server** - Uses `a2a-go` library's server
3. **Task Storage** - Relies on A2A server's task management
4. **Authentication** - Handled by A2A server layer
5. **Push Notifications** - Not implemented
6. **WebSocket Support** - Handled by A2A server

## Key Takeaways for Rust Implementation

### Minimal Scope (What Go Actually Does):

1. ✅ **AgentCard Generation**
   - `BuildAgentSkills()` function
   - Skill extraction from agents
   - Recursive sub-agent processing

2. ✅ **Part Conversion**
   - ADK Parts ↔ A2A Parts
   - Special handling for function calls

3. ✅ **Event Conversion**
   - ADK Events → A2A Messages
   - A2A Events → ADK Events

4. ✅ **Executor**
   - Runs ADK agent in response to A2A request
   - Streams results as A2A artifact updates
   - Manages task state transitions

5. ✅ **Metadata Management**
   - Tracks invocation context
   - Preserves escalate/transfer actions

### What We DON'T Need to Implement:

- ❌ Full A2A server (use library)
- ❌ Task storage (use library)
- ❌ Authentication (use library)
- ❌ HTTP endpoints (except AgentCard)
- ❌ WebSocket transport

### Recommended Rust Implementation:

**Option 1: Full Integration (Like Go)**
- Use `a2a-rs` library for server
- Implement Executor trait
- Implement all conversions
- **Effort**: 8-10 hours

**Option 2: Minimal (Just AgentCard)**
- Only implement `BuildAgentSkills()` equivalent
- Single endpoint: `GET /a2a/agents/:app_name/card`
- No full A2A server integration
- **Effort**: 2-3 hours

**Recommendation**: Start with Option 2 (minimal), expand later if needed.

## Summary

The Go implementation is a **complete A2A integration** that:
- Converts ADK agents to A2A format
- Executes agents in response to A2A requests
- Streams results as A2A events
- Manages full request/response lifecycle

For Rust ADK, we can start with **just AgentCard generation** (10% of Go implementation) and expand later based on actual usage needs.
