# Events

Events are the fundamental building blocks of conversation history in ADK-Rust. Each interaction with an agent—whether it's a user message, an agent response, or a tool execution—is recorded as an event. Events form an immutable log that captures the complete execution trace of an agent session.

## Overview

The event system serves several critical purposes:

- **Conversation History**: Events form the chronological record of all interactions in a session
- **State Management**: Events carry state changes through the `state_delta` field
- **Artifact Tracking**: Events record artifact operations through the `artifact_delta` field
- **Agent Coordination**: Events enable agent transfers and escalations
- **Debugging & Observability**: Events provide a complete audit trail of agent behavior

## Event Structure

An `Event` represents a single interaction in a conversation. ADK-Rust uses a unified Event type that embeds `LlmResponse`, matching the design pattern used in ADK-Go:

```rust
pub struct Event {
    pub id: String,                    // Unique event identifier (UUID)
    pub timestamp: DateTime<Utc>,      // When the event occurred
    pub invocation_id: String,         // Links related events in a single invocation
    pub branch: String,                // For future branching support
    pub author: String,                // Who created this event (user, agent name, tool name)
    pub llm_response: LlmResponse,     // Contains content and LLM metadata
    pub actions: EventActions,         // Side effects and metadata
    pub long_running_tool_ids: Vec<String>,  // IDs of long-running tools
}
```

The `LlmResponse` struct contains:
```rust
pub struct LlmResponse {
    pub content: Option<Content>,      // The message content (text, parts, etc.)
    pub usage_metadata: Option<UsageMetadata>,
    pub finish_reason: Option<FinishReason>,
    pub partial: bool,                 // True for streaming partial responses
    pub turn_complete: bool,           // True when the turn is complete
    pub interrupted: bool,             // True if generation was interrupted
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}
```

Content is accessed consistently via `event.llm_response.content`:
```rust
if let Some(content) = &event.llm_response.content {
    for part in &content.parts {
        if let Part::Text { text } = part {
            println!("{}", text);
        }
    }
}
```

### Key Fields

- **id**: A unique UUID identifying this specific event. Used for event retrieval and ordering.

- **timestamp**: The UTC timestamp when the event was created. Events are ordered chronologically in the session.

- **invocation_id**: Groups events that belong to the same agent invocation. When an agent processes a message, all events generated (agent response, tool calls, sub-agent calls) share the same invocation_id.

- **branch**: Reserved for future branching functionality. Currently unused but allows for conversation branching in future versions.

- **author**: Identifies who created the event:
  - User messages: typically "user" or a user identifier
  - Agent responses: the agent's name
  - Tool executions: the tool's name
  - System events: "system"

- **llm_response**: Contains the message content and LLM metadata. Access content via `event.llm_response.content`. The `Content` type can contain text, multimodal parts (images, audio), or structured data. Some events (like pure state updates) may have `content: None`.

## EventActions

The `EventActions` struct contains metadata and side effects associated with an event:

```rust
pub struct EventActions {
    pub state_delta: HashMap<String, Value>,    // State changes to apply
    pub artifact_delta: HashMap<String, i64>,   // Artifact version changes
    pub skip_summarization: bool,               // Skip this event in summaries
    pub transfer_to_agent: Option<String>,      // Transfer control to another agent
    pub escalate: bool,                         // Escalate to human or supervisor
}
```

### state_delta

The `state_delta` field contains key-value pairs representing changes to the session state. When an event is appended to a session, these changes are merged into the session's state.

State keys can use prefixes to control scope:
- `app:key` - Application-scoped state (shared across all users)
- `user:key` - User-scoped state (shared across all sessions for a user)
- `temp:key` - Temporary state (cleared between invocations)
- No prefix - Session-scoped state (default)

Example:
```rust
let mut actions = EventActions::default();
actions.state_delta.insert("user_name".to_string(), json!("Alice"));
actions.state_delta.insert("temp:current_step".to_string(), json!(3));
```

### artifact_delta

The `artifact_delta` field tracks changes to artifacts. Keys are artifact names, and values are version numbers. This allows the system to track which artifacts were created or modified during an event.

Example:
```rust
actions.artifact_delta.insert("report.pdf".to_string(), 1);
actions.artifact_delta.insert("chart.png".to_string(), 2);
```

### skip_summarization

When `true`, this event will be excluded from conversation summaries. Useful for internal events, debugging information, or verbose tool outputs that shouldn't be part of the main conversation flow.

### transfer_to_agent

When set to an agent name, control is transferred to that agent. This enables multi-agent workflows where one agent can hand off to another. The target agent must be configured as a sub-agent.

Example:
```rust
actions.transfer_to_agent = Some("specialist_agent".to_string());
```

### escalate

When `true`, signals that the conversation should be escalated to a human operator or supervisor agent. The specific escalation behavior depends on your application's implementation.

## Conversation History Formation

Events form the conversation history by accumulating in chronological order within a session. When an agent processes a request:

1. **User Message Event**: A new event is created with the user's input
2. **Agent Processing**: The agent receives the conversation history (all previous events)
3. **Agent Response Event**: The agent's response is recorded as a new event
4. **Tool Execution Events**: Each tool call may generate additional events
5. **State Updates**: State deltas from all events are merged into the session state

The conversation history is constructed by:
- Retrieving all events from the session in chronological order
- Converting each event's content into the appropriate format for the LLM
- Including state information from accumulated state deltas
- Filtering out events marked with `skip_summarization` when appropriate

### Event Flow Example

```
Session Start
  ↓
[Event 1] User: "What's the weather in Tokyo?"
  ↓
[Event 2] Agent: "Let me check that for you."
  ↓
[Event 3] Tool (weather_api): {"temp": 22, "condition": "sunny"}
  ↓
[Event 4] Agent: "It's 22°C and sunny in Tokyo."
  ↓
Session State Updated
```

Each event builds on the previous ones, creating a complete audit trail of the conversation.

## Working with Events

### Accessing Events from a Session

```rust
use adk_rust::session::{SessionService, GetRequest};

// Retrieve a session with its events
let session = session_service.get(GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: session_id.clone(),
    num_recent_events: None,  // Get all events
    after: None,
}).await?;

// Access the events
let events = session.events();
println!("Total events: {}", events.len());

// Iterate through events (note: session events use llm_response.content)
for i in 0..events.len() {
    if let Some(event) = events.at(i) {
        println!("Event {}: {} by {} at {}",
            event.id,
            event.llm_response.content.as_ref().map(|_| "has content").unwrap_or("no content"),
            event.author,
            event.timestamp
        );
    }
}
```

### Inspecting Event Details

```rust
// Get a specific event from session (uses llm_response.content)
if let Some(event) = events.at(0) {
    // Check the author
    println!("Author: {}", event.author);

    // Check content (session events use llm_response.content)
    if let Some(content) = &event.llm_response.content {
        for part in &content.parts {
            if let Part::Text { text } = part {
                println!("Text: {}", text);
            }
        }
    }

    // Check for state changes
    if !event.actions.state_delta.is_empty() {
        println!("State changes:");
        for (key, value) in &event.actions.state_delta {
            println!("  {} = {}", key, value);
        }
    }

    // Check for agent transfers
    if let Some(target) = &event.actions.transfer_to_agent {
        println!("Transfers to: {}", target);
    }

    // Check for artifacts
    if !event.actions.artifact_delta.is_empty() {
        println!("Artifacts modified:");
        for (name, version) in &event.actions.artifact_delta {
            println!("  {} (v{})", name, version);
        }
    }
}
```

### Limiting Event History

For long conversations, you may want to retrieve only recent events:

```rust
// Get only the last 10 events
let session = session_service.get(GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: session_id.clone(),
    num_recent_events: Some(10),
    after: None,
}).await?;
```

## How Events Flow: Generation and Processing

Understanding how events are created and processed helps clarify how the framework manages actions and history.

### Generation Sources

Events are created at different points in the agent execution lifecycle:

1. **User Input**: The Runner wraps user messages into an Event with `author = "user"`
2. **Agent Responses**: Agents yield Event objects (setting `author = agent.name()`) to communicate responses
3. **LLM Output**: The model integration layer translates LLM output (text, function calls) into Event objects
4. **Tool Results**: After tool execution, the framework generates an Event containing the tool response

### Processing Flow

When an event is generated, it follows this processing path:

1. **Generation**: An event is created and yielded by its source (agent, tool, or user input handler)
2. **Runner Receives**: The Runner executing the agent receives the event
3. **SessionService Processing**: The Runner sends the event to the SessionService, which:
   - **Applies Deltas**: Merges `state_delta` into session state and updates artifact records
   - **Finalizes Metadata**: Assigns unique `id` if not present, sets `timestamp`
   - **Persists to History**: Appends the event to `session.events`
4. **Stream Output**: The Runner yields the processed event to the calling application

This flow ensures that state changes and history are consistently recorded alongside the communication content.

```rust
// Conceptual flow
User Input → Runner → Agent → LLM → Event Generated
                                         ↓
                                   SessionService
                                   - Apply state_delta
                                   - Record in history
                                         ↓
                                   Event Stream → Application
```

## Identifying Event Types

When processing events from the Runner, you'll want to identify what type of event you're dealing with:

### By Author

The `author` field tells you who created the event:

```rust
match event.author.as_str() {
    "user" => println!("User input"),
    agent_name => println!("Response from agent: {}", agent_name),
}
```

### By Content Type

Check the `llm_response.content` field to determine the payload type:

```rust
if let Some(content) = &event.llm_response.content {
    // Check for text content
    let has_text = content.parts.iter().any(|part| {
        matches!(part, Part::Text { .. })
    });

    // Check for function calls (tool requests)
    let has_function_call = content.parts.iter().any(|part| {
        matches!(part, Part::FunctionCall { .. })
    });

    // Check for function responses (tool results)
    let has_function_response = content.parts.iter().any(|part| {
        matches!(part, Part::FunctionResponse { .. })
    });

    if has_text {
        println!("Text message");
    } else if has_function_call {
        println!("Tool call request");
    } else if has_function_response {
        println!("Tool result");
    }
}
```

### By Actions

Check the `actions` field for control signals and side effects:

```rust
// State changes
if !event.actions.state_delta.is_empty() {
    println!("Event contains state changes");
}

// Agent transfer
if let Some(target) = &event.actions.transfer_to_agent {
    println!("Transfer to agent: {}", target);
}

// Escalation signal
if event.actions.escalate {
    println!("Escalation requested");
}

// Skip summarization
if event.actions.skip_summarization {
    println!("Skip this event in summaries");
}
```

## Working with Event Streams

When running an agent, you receive a stream of events. Here's how to process them effectively:

### Processing Events from Runner

```rust
use futures::StreamExt;

let mut stream = runner.run(
    "user_123".to_string(),
    "session_id".to_string(),
    user_input,
).await?;

while let Some(event_result) = stream.next().await {
    match event_result {
        Ok(event) => {
            // Process the event
            println!("Event from: {}", event.author);

            // Extract text content
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        print!("{}", text);
                    }
                }
            }

            // Check for state changes
            if !event.actions.state_delta.is_empty() {
                println!("\nState updated: {:?}", event.actions.state_delta);
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            break;
        }
    }
}
```

### Extracting Function Calls

When the LLM requests a tool, the event contains function call information:

```rust
if let Some(content) = &event.llm_response.content {
    for part in &content.parts {
        if let Part::FunctionCall { name, args } = part {
            println!("Tool requested: {}", name);
            println!("Arguments: {}", args);

            // Your application might dispatch tool execution here
            // based on the tool name and arguments
        }
    }
}
```

### Extracting Function Responses

After a tool executes, the result is returned in a function response:

```rust
if let Some(content) = &event.llm_response.content {
    for part in &content.parts {
        if let Part::FunctionResponse { function_response, .. } = part {
            println!("Tool result from: {}", function_response.name);
            println!("Response: {}", function_response.response);

            // Process the tool result
            // The LLM will use this to continue the conversation
        }
    }
}
```

## Common Event Patterns

Here are typical event sequences you'll encounter:

### Simple Text Exchange

```
[Event 1] author="user", content=Text("Hello")
[Event 2] author="assistant", content=Text("Hi! How can I help?")
```

### Tool Usage Flow

```
[Event 1] author="user", content=Text("What's the weather?")
[Event 2] author="assistant", content=FunctionCall(name="get_weather", args={...})
[Event 3] author="assistant", content=FunctionResponse(name="get_weather", response={...})
[Event 4] author="assistant", content=Text("It's sunny and 72°F")
```

### State Update

```
[Event 1] author="assistant", content=Text("I've saved your preference")
           actions.state_delta={"user_theme": "dark"}
```

### Agent Transfer

```
[Event 1] author="router", content=Text("Transferring to specialist")
           actions.transfer_to_agent=Some("specialist_agent")
[Event 2] author="specialist_agent", content=Text("I can help with that")
```

## Event Metadata and Identifiers

### Event ID

Each event has a unique `id` (UUID) for precise identification:

```rust
println!("Event ID: {}", event.id);
```

### Invocation ID

The `invocation_id` groups all events from a single user request through to the final response:

```rust
// All events in one interaction share the same invocation_id
println!("Invocation: {}", event.invocation_id);

// Use this for logging and tracing
log::info!("Processing event {} in invocation {}", event.id, event.invocation_id);
```

### Timestamp

Events are timestamped for chronological ordering:

```rust
println!("Event occurred at: {}", event.timestamp.format("%Y-%m-%d %H:%M:%S"));
```

## Best Practices

1. **Event Immutability**: Events should never be modified after creation. They form an immutable audit log.

2. **State Management**: Use `state_delta` for all state changes rather than modifying state directly. This ensures changes are tracked in the event log.

3. **Meaningful Authors**: Set clear, descriptive author names to make event logs easier to understand.

4. **Selective Summarization**: Use `skip_summarization` for verbose or internal events that would clutter the conversation history.

5. **Invocation Grouping**: Keep the same `invocation_id` for all events generated during a single agent invocation to maintain logical grouping.

6. **Artifact Tracking**: Always update `artifact_delta` when creating or modifying artifacts to maintain consistency.

7. **Stream Processing**: Always handle errors when processing event streams. Events can fail due to LLM errors, tool failures, or network issues.

8. **Content Checking**: Always check if `llm_response.content` is `Some` before accessing parts. Some events (like pure state updates) may not have content.

9. **Pattern Matching**: Use Rust's pattern matching to elegantly handle different event types and content parts.

10. **Logging**: Use `invocation_id` to correlate all events within a single user interaction for debugging and observability.

## Related Documentation

- [Sessions](../sessions/sessions.md) - Session management and lifecycle
- [State Management](../sessions/state.md) - Working with session state
- [Artifacts](../artifacts/artifacts.md) - Managing binary data
- [Multi-Agent Systems](../agents/multi-agent.md) - Agent transfers and coordination
- [Callbacks](../callbacks/callbacks.md) - Intercepting and modifying events


---

**Previous**: [← Artifacts](../artifacts/artifacts.md) | **Next**: [Telemetry →](../observability/telemetry.md)
