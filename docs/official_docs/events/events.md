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
use adk_rust::{EventActions, LlmResponse};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub struct Event {
    pub id: String,                    // Unique event identifier (UUID)
    pub timestamp: DateTime<Utc>,      // When the event occurred
    pub invocation_id: String,         // Links related events in a single invocation
    pub branch: String,                // Conversation branch (see below)
    pub author: String,                // Who created this event ("user" or an agent name)
    pub llm_response: LlmResponse,     // Contains content and LLM metadata (flattened when serialized)
    pub actions: EventActions,         // Side effects and metadata
    pub long_running_tool_ids: Vec<String>,  // IDs of long-running tools
    pub llm_request: Option<String>,   // The serialized model request, when recorded
    pub provider_metadata: HashMap<String, String>,  // Event-level metadata, serialized as `event_metadata`
}
```

The `LlmResponse` struct contains:
```rust
use adk_rust::{CitationMetadata, Content, FinishReason, UsageMetadata};

pub struct LlmResponse {
    pub content: Option<Content>,      // The message content (text, parts, etc.)
    pub usage_metadata: Option<UsageMetadata>,
    pub finish_reason: Option<FinishReason>,
    pub citation_metadata: Option<CitationMetadata>,
    pub partial: bool,                 // True for streaming partial responses
    pub turn_complete: bool,           // True when the turn is complete
    pub interrupted: bool,             // True if generation was interrupted
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub provider_metadata: Option<serde_json::Value>,  // Provider-specific response metadata
    pub interaction_id: Option<String>,  // Gemini Interactions API interaction ID
    pub model: Option<String>,         // The model that produced the response, when known
    pub provider: Option<String>,      // The provider that served it, when known
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

- **branch**: The conversation branch the event was produced on. `ParallelAgent` places each sub-agent on its own branch (`{parent}.{parallel_agent}.{sub_agent}`) and stamps the events it emits, so history reads scoped to a branch exclude what sibling branches produced. An event is visible from a branch when its own branch equals that branch or is an *ancestor* of it; siblings and nested descendants are not. An empty branch means "unscoped" and stays visible everywhere, so events written without one are unaffected. See [Workflow Agents](../agents/workflow-agents.md#parallelagent).

- **author**: Identifies who created the event:
  - User messages: `"user"`
  - Agent responses: the agent's name
  - Tool calls and tool results: the name of the agent that issued the call

- **llm_response**: Contains the message content and LLM metadata. Access content via `event.llm_response.content`. The `Content` type can contain text, multimodal parts (images, audio), or structured data. Some events (like pure state updates) may have `content: None`.

- **llm_response.error_code / error_message**: Set when the provider reported a terminal failure for the turn. Providers can report a failure inside an otherwise successful stream item — `adk-anthropic`'s stream errors, the OpenAI Responses error event, and the OpenAI websocket transport all do — so these fields are how a failed turn is distinguished from an empty one. `LlmAgent` emits the event carrying them *and then* ends the run with an `AdkError` whose code is `model.provider_error`, with the provider's own code in the error details under `provider_error_code`. The event is emitted first so the failed turn is still observable and persisted rather than disappearing into an error.

  Truncation is **not** reported this way: a response cut short by a token limit carries `finish_reason: FinishReason::MaxTokens` and no error code.

- **llm_response.interrupted**: The generation was interrupted (for example a realtime turn cut off by the user, or a tool confirmation pausing the turn). This is recorded on the event but is **not** a terminal error on its own, so the run does not fail.

```rust
// Distinguishing a failed turn from an empty one
if let Some(code) = &event.llm_response.error_code {
    let detail = event.llm_response.error_message.as_deref().unwrap_or("no message");
    eprintln!("provider reported {code}: {detail}");
}
```

## EventActions

The `EventActions` struct contains metadata and side effects associated with an event:

```rust
use adk_rust::{EventCompaction, ToolConfirmationDecision, ToolConfirmationRequest, WorkflowPosition};
use serde_json::Value;
use std::collections::HashMap;

pub struct EventActions {
    pub state_delta: HashMap<String, Value>,    // State changes to apply
    pub artifact_delta: HashMap<String, i64>,   // Artifact version changes
    pub skip_summarization: bool,               // Skip this event in summaries
    pub transfer_to_agent: Option<String>,      // Transfer control to another agent
    pub escalate: bool,                         // End the enclosing loop or fan-out
    pub tool_confirmation: Option<ToolConfirmationRequest>,      // Pending tool confirmation
    pub tool_confirmation_decision: Option<ToolConfirmationDecision>, // Approval/denial
    pub compaction: Option<EventCompaction>,     // Context compaction summary
    pub route: Option<Vec<String>>,              // Graph route targets
    pub input_screened: bool,                    // User message screened before it was saved
    pub workflow_positions: HashMap<String, WorkflowPosition>, // Where workflows stopped on a held call
}
```

### state_delta

The `state_delta` field contains key-value pairs representing changes to the session state. When an event is appended to a session, these changes are merged into the session's state.

State keys can use prefixes to control scope:
- `app:key` - Application-scoped state (shared across all users)
- `user:key` - User-scoped state (shared across all sessions for a user)
- `temp:key` - Temporary state: visible to later tools, transferred agents and `{temp:key}` instructions for the rest of the invocation, and never persisted by the session service
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

Set by a tool on its result (through `ToolContext::set_actions`) to end the agent's turn on that
result: `LlmAgent` does not call the model again to summarize it, and the tool-result event is
the turn's final response (`is_final_response()` returns `true`). `AgentTool::skip_summarization(true)`
sets it for a delegation.

### transfer_to_agent

When set to an agent name, control is transferred to that agent. This enables multi-agent workflows where one agent can hand off to another. The runner checks every transfer, whether the model's `transfer_to_agent` call, a tool or a custom agent requested it: the target must be one of the source agent's sub-agents, its parent (unless `disallow_transfer_to_parent`) or its parent's other sub-agents (unless `disallow_transfer_to_peers`). A refused transfer is logged and control stays with the source. See [Transfer Restrictions](../agents/multi-agent.md#transfer-restrictions).

Example:
```rust
actions.transfer_to_agent = Some("specialist_agent".to_string());
```

### escalate

When `true`, ends the enclosing workflow step: a `LoopAgent` stops iterating, and a `ParallelAgent` stops its other branches and passes the escalation on. `ExitLoopTool` sets it. Like `skip_summarization`, an escalating tool result also ends the agent's turn.

### input_screened

Set on a user message that the Runner screened with its agents' input guardrails before saving it. A saved user message without it — saved by an earlier release, or appended to the session directly — is screened again before any model sees it. See [Guardrails](../security/guardrails.md#input-guardrails-and-the-session).

## Conversation History Formation

Events form the conversation history by accumulating in chronological order within a session. When an agent processes a request:

1. **User Message Event**: A new event is created with the user's input
2. **Agent Processing**: The agent receives the conversation history (all previous events)
3. **Agent Response Event**: The agent's response is recorded as a new event
4. **Tool Execution Events**: Each tool call may generate additional events
5. **State Updates**: State deltas from all events are merged into the session state

`LlmAgent` builds each model request from its instructions, sent as `system`-role contents,
followed by the session's events in order, projected for the agent that asks:

| Events | What the agent's request carries |
|--------|----------------------------------|
| The user's messages and the agent's own turns | Their own roles (`user`, `model`, function calls and responses) |
| Another agent's text, calls and results, as in a workflow | User-role context: one message per event, opening with `adk_core::history::OTHER_AGENT_CONTEXT_PREAMBLE` and quoting the payload between `<<<BEGIN_QUOTED_AGENT_CONTENT>>>` and `<<<END_QUOTED_AGENT_CONTENT>>>`; the other agent's thinking is left out |
| Another agent's turns, for an agent that received control by transfer | Left out |
| Events of a sibling `ParallelAgent` branch | Left out (see `branch` above) |
| Events a compaction summarized | The compaction summary |
| A held call's confirmation-pending response, once the call is resumed | Replaced by the call's real response |
| A function response whose call is no longer in the history | Left out, so every response has its call |

A request whose history would end on a model answer, such as a loop running the agent again
after its own answer, gets a short user turn, since models without assistant prefill reject a
request that ends on one. `RunConfig::history_max_events` limits how many recent persisted
events a run loads. Partial (streamed) events are not persisted, so they never reach the history.

### Event Flow Example

```
Session Start
  ↓
[Event 1] author "user":          "What's the weather in Tokyo?"
  ↓
[Event 2] author "weather_agent": FunctionCall get_weather({"city": "Tokyo"})
  ↓
[Event 3] author "weather_agent": FunctionResponse get_weather → {"temp": 22, "condition": "sunny"}
  ↓
[Event 4] author "weather_agent": "It's 22°C and sunny in Tokyo."
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
use adk_rust::session::{GetRequest, SessionService};

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

1. **User Input**: The Runner wraps user messages into an Event with `author = "user"`, after the agents' input guardrails screen it
2. **Agent Responses**: Agents yield Event objects (setting `author = agent.name()`) to communicate responses
3. **LLM Output**: The model integration layer translates LLM output (text, function calls) into Event objects
4. **Tool Results**: After tool execution, the framework generates an Event containing the tool response

### Processing Flow

When an event is generated, it follows this processing path:

1. **Generation**: An event is created and yielded by its source (agent, tool, or user input handler)
2. **Runner Receives**: The Runner executing the agent receives the event
3. **SessionService Processing**: The Runner sends each complete (non-partial) event to the SessionService, which:
   - **Applies Deltas**: Merges `state_delta` into session state, leaving out `temp:` keys
   - **Persists to History**: Appends the event to `session.events`
4. **Stream Output**: The Runner yields the event to the calling application. Partial (streamed) events are yielded without being persisted

This flow ensures that state changes and history are consistently recorded alongside the communication content.

```text
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

// Context compaction
if let Some(compaction) = &event.actions.compaction {
    println!("Compacted events from {} to {}",
        compaction.start_timestamp, compaction.end_timestamp);
}

// Tool confirmation
if let Some(confirmation) = &event.actions.tool_confirmation {
    println!("Tool {} awaiting confirmation", confirmation.tool_name);
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
use adk_rust::futures::StreamExt;
use adk_rust::{EventTextDeltas, SessionId, UserId};

let mut stream = runner.run(
    UserId::new("user_123")?,
    SessionId::new("session_id")?,
    user_input,
).await?;

// A streamed answer arrives as partial deltas and then one complete event with the whole
// text; EventTextDeltas reduces that complete event to the text not yet printed.
let mut text_deltas = EventTextDeltas::default();
while let Some(event_result) = stream.next().await {
    match event_result {
        Ok(event) => {
            // Process the event
            println!("Event from: {}", event.author);

            // Extract text content
            let event = text_deltas.push(&event);
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
        if let Part::FunctionCall { name, args, thought_signature, .. } = part {
            println!("Tool requested: {}", name);
            println!("Arguments: {}", args);
            if let Some(sig) = thought_signature {
                println!("Thought signature: {} bytes", sig.len());
            }

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

### First-Class Tool Accessors (recommended)

Walking `content.parts` and matching `Part` variants works, but for building UIs
the `Event` type exposes typed, render-ready accessors so you never touch part
internals. They are the recommended way to consume tool activity from an
`EventStream`:

```rust
// A model requested one or more tools.
for call in event.tool_calls() {
    println!("→ {} {}", call.name, call.args);     // call.call_id correlates this call
}

// A tool finished — surfaces the output of ANY tool, streaming or not.
for result in event.tool_results() {
    println!("✓ {} returned {}", result.name, result.response);
}
```

`tool_calls()` returns `Vec<ToolCallView>` and `tool_results()` returns
`Vec<ToolResultView>`:

```rust
use serde_json::Value;

pub struct ToolCallView<'a>   { pub call_id: Option<&'a str>, pub name: &'a str, pub args: &'a Value }
pub struct ToolResultView<'a> { pub call_id: Option<&'a str>, pub name: &'a str, pub response: &'a Value }
```

Both views are empty for events that carry no tool activity, so a UI can call
them on every event unconditionally. `tool_results()` is what makes a tool's
output **first-class**: a one-shot tool (`read_file`, a weather API) has no
streaming output, but its result still arrives as an ordinary event you can
render — not just `bash`-style tools.

**Correlation.** Use `call_id` to attach a result (and any progress chunks, see
below) back to its originating call. When a provider sends a call without an ID (Ollama,
tool-call markup in text), `LlmAgent` assigns one starting with `adk-call-`
(`adk_core::GENERATED_CALL_ID_PREFIX`) in the saved event and its response, and removes it again
from model requests.

### Complete response snapshots

Partial model events carry live deltas. A terminal event marked with
`llm_response.provider_metadata.content_complete = true` contains the complete
response for the same event ID, including text already streamed. Persist the
original terminal event, or replace the matching partial projection with it.

Append-only consumers can keep one `adk_core::EventTextDeltas` per stream and
call `push(&event)` before rendering. The adapter removes previously emitted
text and thinking prefixes while preserving tool parts, signatures, actions,
and usage. It leaves unmarked deltas unchanged. A corrected snapshot that no
longer starts with the emitted prefix is returned in full; append-only output
cannot retract earlier content.

### Lifecycle markers

Two markers describe work in progress rather than a response. Neither event is
a final response, so `is_final_response()` returns `false` for both.

| Marker | Location | Meaning |
|--------|----------|---------|
| `adk_tool_started` | `event.provider_metadata` (event-level) | A tool call began executing. The value is a JSON string `{"id": <call id or null>, "name": <tool name>}`. The event has no content and is `partial = true`, so the runner does not persist it. |
| `continue_turn` | `llm_response.provider_metadata` | A provider-native tool paused the turn without a client function call. `LlmAgent` sends another model request within the same `max_iterations` budget, skips output guardrails and `output_key` for the paused response, and keeps it in conversation history. |

Tool-result events are emitted as each call completes, so with parallel dispatch
they arrive in completion order. The model receives the results in call order.

```rust
use adk_core::Event;

fn tool_started(event: &Event) -> Option<serde_json::Value> {
    event
        .provider_metadata
        .get("adk_tool_started")
        .and_then(|value| serde_json::from_str(value).ok())
}
```

### Streaming Tool Progress

Long-running tools (a shell command, a build, a download) can push intermediate
output to the UI *while still executing*, rather than making the user wait for
the final result. A tool calls
[`ToolContext::emit_progress`](../tools/function-tools.md#streaming-progress-from-a-tool):

```rust
// inside Tool::execute, as output arrives:
ctx.emit_progress("stdout", &format!("{line}\n")).await;
ctx.emit_progress("stderr", &warning).await;
```

The framework forwards each chunk as a **partial event on the same
`EventStream`** as everything else — no separate channel, no log scraping.
Detect and route these events with `tool_progress_stream()`:

```rust
while let Some(event) = stream.next().await {
    let event = event?;
    if let Some(stream_name) = event.tool_progress_stream() {   // "stdout" | "stderr" | custom
        // render a live terminal line from event.llm_response.content (role "tool")
        continue;
    }
    // ... handle tool_calls(), tool_results(), and model text as usual
}
```

Progress events are marked `partial = true` (so they are never mistaken for a
final response) and carry the originating call id in `provider_metadata`
(key `adk_core::TOOL_PROGRESS_CALL_ID_KEY`) so you can group them with the right
`tool_calls()` / `tool_results()` entry. The default `emit_progress`
implementation is a no-op, so tools that don't stream are unaffected.

Putting it together, a complete tool lifecycle on the stream looks like:

```
tool_calls()          →  open a card  (name + args, correlated by call_id)
tool_progress_stream()→  live stdout/stderr lines stream into the card
tool_results()        →  finalize the card with the tool's output
```

This is exactly the pattern the `streaming_bash` example uses to render both
streaming (`bash`) and one-shot (`read_file`, `grep`, `glob`) tools from a
single web-socket feed.

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
[Event 1] author="router", content=FunctionCall(name="transfer_to_agent", args={"agent_name": "specialist_agent"})
[Event 2] author="router", content=FunctionResponse(name="transfer_to_agent", response={...})
           actions.transfer_to_agent=Some("specialist_agent")
[Event 3] author="specialist_agent", content=Text("I can help with that")
```

The event that carries `transfer_to_agent` also answers every other call of the same model
turn, which the transfer skips, so each call in the history has exactly one response.

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
tracing::info!(event.id = %event.id, invocation.id = %event.invocation_id, "processing event");
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

4. **Ending a Turn on a Tool Result**: Set `skip_summarization` when a tool's result is the answer, so the agent does not call the model again to restate it.

5. **Invocation Grouping**: Keep the same `invocation_id` for all events generated during a single agent invocation to maintain logical grouping.

6. **Artifact Tracking**: Always update `artifact_delta` when creating or modifying artifacts to maintain consistency.

7. **Stream Processing**: Always handle errors when processing event streams. Events can fail due to LLM errors, tool failures, or network issues.

8. **Content Checking**: Always check if `llm_response.content` is `Some` before accessing parts. Some events (like pure state updates) may not have content.

9. **Pattern Matching**: Use Rust's pattern matching to elegantly handle different event types and content parts.

10. **Logging**: Use `invocation_id` to correlate all events within a single user interaction for debugging and observability.

## Related Documentation

- [Sessions](../sessions/sessions.md) - Session management and lifecycle
- [State Management](../sessions/state.md) - Working with session state
- [Context Compaction](../sessions/context-compaction.md) - Sliding-window event summarization
- [Artifacts](../artifacts/artifacts.md) - Managing binary data
- [Multi-Agent Systems](../agents/multi-agent.md) - Agent transfers and coordination
- [Callbacks](../callbacks/callbacks.md) - Intercepting and modifying events


---

**Previous**: [← Artifacts](../artifacts/artifacts.md) | **Next**: [Telemetry →](../observability/telemetry.md)
