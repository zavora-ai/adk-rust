# Core Types

Fundamental types and traits from `adk-core` that form the foundation of ADK-Rust.

## Content and Part

Every message in ADK flows through `Content` and `Part`. Understanding these types is essential for working with agents, tools, and callbacks.

### Content

`Content` represents a single message in a conversation. It has a `role` (who sent it) and one or more `parts` (the actual content).

**Roles:**
- `"user"` - Messages from the user
- `"model"` - Responses from the AI model
- `"tool"` - Results from tool execution

```rust
use adk_core::Content;

// Simple text message from user
let user_msg = Content::new("user")
    .with_text("What's the weather like?");

// Model response
let model_msg = Content::new("model")
    .with_text("The weather is sunny and 72°F.");

// Multiple text parts in one message
let detailed_msg = Content::new("user")
    .with_text("Here's my question:")
    .with_text("What is the capital of France?");
```

**Multimodal Content:**

Content can include images, audio, PDFs, and other binary data alongside text:

```rust
// Image from bytes (e.g., read from file)
let image_bytes = std::fs::read("photo.jpg")?;
let content = Content::new("user")
    .with_text("What's in this image?")
    .with_inline_data("image/jpeg", image_bytes);

// Image from URL (model fetches it)
let content = Content::new("user")
    .with_text("Describe this image")
    .with_file_uri("image/png", "https://example.com/chart.png");

// PDF document
let pdf_bytes = std::fs::read("report.pdf")?;
let content = Content::new("user")
    .with_text("Summarize this document")
    .with_inline_data("application/pdf", pdf_bytes);
```

### Part

`Part` is an enum representing different types of content within a message:

```rust
pub enum Part {
    // Plain text
    Text { text: String },
    
    // Binary data embedded in the message
    InlineData { mime_type: String, data: Vec<u8> },
    
    // Reference to external file (URL or cloud storage)
    FileData { mime_type: String, file_uri: String },
    
    // Model requesting a tool call
    FunctionCall { name: String, args: Value, id: Option<String> },
    
    // Result of a tool execution
    FunctionResponse { function_response: FunctionResponseData, id: Option<String> },
}
```

**Creating Parts Directly:**

```rust
use adk_core::Part;

// Text part
let text = Part::text_part("Hello, world!");

// Image from bytes
let image = Part::inline_data("image/png", png_bytes);

// Image from URL
let remote_image = Part::file_data("image/jpeg", "https://example.com/photo.jpg");
```

**Inspecting Parts:**

```rust
// Get text content (returns None for non-text parts)
if let Some(text) = part.text() {
    println!("Text: {}", text);
}

// Get MIME type (for InlineData and FileData)
if let Some(mime) = part.mime_type() {
    println!("MIME type: {}", mime);
}

// Get file URI (for FileData only)
if let Some(uri) = part.file_uri() {
    println!("File URI: {}", uri);
}

// Check if part contains media (image, audio, video, etc.)
if part.is_media() {
    println!("This part contains binary media");
}
```

**Iterating Over Parts:**

```rust
for part in &content.parts {
    match part {
        Part::Text { text } => println!("Text: {}", text),
        Part::InlineData { mime_type, data } => {
            println!("Binary data: {} ({} bytes)", mime_type, data.len());
        }
        Part::FileData { mime_type, file_uri } => {
            println!("File: {} at {}", mime_type, file_uri);
        }
        Part::FunctionCall { name, args, .. } => {
            println!("Tool call: {}({})", name, args);
        }
        Part::FunctionResponse { function_response, .. } => {
            println!("Tool result: {} -> {}", 
                function_response.name, 
                function_response.response
            );
        }
    }
}
```

---

## Agent Trait

The `Agent` trait is the core abstraction for all agents in ADK. Every agent type—LLM agents, workflow agents, custom agents—implements this trait.

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    /// Unique identifier for this agent
    fn name(&self) -> &str;
    
    /// Human-readable description of what this agent does
    fn description(&self) -> &str;
    
    /// Child agents (for workflow agents like Sequential, Parallel)
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    
    /// Execute the agent and return a stream of events
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}
```

**Key Points:**

- `name()`: Used for logging, transfers, and identification. Must be unique within a multi-agent system.
- `description()`: Shown to LLMs when the agent is used as a tool or for routing decisions.
- `sub_agents()`: Returns child agents. Empty for leaf agents (LlmAgent), populated for containers (SequentialAgent, ParallelAgent).
- `run()`: The main execution method. Receives context and returns a stream of events.

**Why EventStream?**

Agents return `EventStream` (a stream of `Result<Event>`) rather than a single response because:
1. **Streaming**: Responses can be streamed token-by-token for better UX
2. **Tool calls**: Multiple tool calls and responses happen during execution
3. **State changes**: State updates are emitted as events
4. **Transfers**: Agent transfers are signaled through events

---

## Tool Trait

Tools extend agent capabilities beyond conversation. They let agents interact with APIs, databases, file systems, or perform computations.

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in function calls from the model)
    fn name(&self) -> &str;
    
    /// Description shown to the LLM to help it decide when to use this tool
    fn description(&self) -> &str;
    
    /// JSON Schema defining the expected parameters
    fn parameters_schema(&self) -> Option<Value> { None }
    
    /// JSON Schema defining the response format
    fn response_schema(&self) -> Option<Value> { None }
    
    /// Whether this tool runs asynchronously (returns task ID immediately)
    fn is_long_running(&self) -> bool { false }
    
    /// Execute the tool with given arguments
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}
```

**Key Points:**

- `name()`: The function name the model uses to call this tool. Keep it short and descriptive (e.g., `get_weather`, `search_database`).
- `description()`: Critical for the model to understand when to use the tool. Be specific about what it does and when to use it.
- `parameters_schema()`: JSON Schema that tells the model what arguments to provide. Without this, the model guesses.
- `execute()`: Receives parsed arguments as `serde_json::Value`. Return the result as JSON.

**Implementing a Custom Tool:**

```rust
use adk_core::{Tool, ToolContext, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

struct WeatherTool {
    api_key: String,
}

#[async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &str { 
        "get_weather" 
    }
    
    fn description(&self) -> &str { 
        "Get current weather for a city. Use this when the user asks about weather conditions."
    }
    
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "city": { 
                    "type": "string",
                    "description": "City name (e.g., 'London', 'New York')"
                },
                "units": {
                    "type": "string",
                    "enum": ["celsius", "fahrenheit"],
                    "description": "Temperature units"
                }
            },
            "required": ["city"]
        }))
    }
    
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let city = args["city"].as_str().unwrap_or("Unknown");
        let units = args["units"].as_str().unwrap_or("celsius");
        
        // Call weather API here...
        
        Ok(json!({
            "city": city,
            "temperature": 22,
            "units": units,
            "condition": "sunny"
        }))
    }
}
```

**Long-Running Tools:**

For operations that take a long time (file processing, external API calls), mark the tool as long-running:

```rust
fn is_long_running(&self) -> bool { true }
```

Long-running tools return a task ID immediately. The model is instructed not to call the tool again while it's pending.

---

## Toolset Trait

`Toolset` provides tools dynamically based on context. Use this when:
- Tools depend on user permissions
- Tools are loaded from external sources (MCP servers)
- Tool availability changes during execution

```rust
#[async_trait]
pub trait Toolset: Send + Sync {
    /// Toolset identifier
    fn name(&self) -> &str;
    
    /// Return available tools for the current context
    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>>;
}
```

**Example: Permission-Based Toolset:**

```rust
struct AdminToolset {
    admin_tools: Vec<Arc<dyn Tool>>,
    user_tools: Vec<Arc<dyn Tool>>,
}

#[async_trait]
impl Toolset for AdminToolset {
    fn name(&self) -> &str { "admin_toolset" }
    
    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        if ctx.user_id().starts_with("admin_") {
            Ok(self.admin_tools.clone())
        } else {
            Ok(self.user_tools.clone())
        }
    }
}
```

---

## Context Traits

Contexts provide information and services to agents and tools during execution. There's a hierarchy of context traits, each adding more capabilities.

### ReadonlyContext

Basic information available everywhere:

```rust
pub trait ReadonlyContext: Send + Sync {
    /// Unique ID for this invocation
    fn invocation_id(&self) -> &str;
    
    /// Name of the currently executing agent
    fn agent_name(&self) -> &str;
    
    /// User identifier
    fn user_id(&self) -> &str;
    
    /// Application name
    fn app_name(&self) -> &str;
    
    /// Session identifier
    fn session_id(&self) -> &str;
    
    /// The user's input that triggered this invocation
    fn user_content(&self) -> &Content;
}
```

### CallbackContext

Adds artifact access (extends ReadonlyContext):

```rust
pub trait CallbackContext: ReadonlyContext {
    /// Access to artifact storage (if configured)
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
}
```

### ToolContext

For tool execution (extends CallbackContext):

```rust
pub trait ToolContext: CallbackContext {
    /// ID of the function call that triggered this tool
    fn function_call_id(&self) -> &str;
    
    /// Get current event actions (transfer, escalate, etc.)
    fn actions(&self) -> EventActions;
    
    /// Set event actions (e.g., trigger a transfer)
    fn set_actions(&self, actions: EventActions);
    
    /// Search long-term memory
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}
```

### InvocationContext

Full context for agent execution (extends CallbackContext):

```rust
pub trait InvocationContext: CallbackContext {
    /// The agent being executed
    fn agent(&self) -> Arc<dyn Agent>;
    
    /// Memory service (if configured)
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    
    /// Current session with state and history
    fn session(&self) -> &dyn Session;
    
    /// Execution configuration
    fn run_config(&self) -> &RunConfig;
    
    /// Signal that this invocation should end
    fn end_invocation(&self);
    
    /// Check if invocation has been ended
    fn ended(&self) -> bool;
}
```

---

## Session and State

Sessions track conversations. State stores data within sessions.

### Session

```rust
pub trait Session: Send + Sync {
    /// Unique session identifier
    fn id(&self) -> &str;
    
    /// Application this session belongs to
    fn app_name(&self) -> &str;
    
    /// User who owns this session
    fn user_id(&self) -> &str;
    
    /// Mutable state storage
    fn state(&self) -> &dyn State;
    
    /// Previous messages in this conversation
    fn conversation_history(&self) -> Vec<Content>;
}
```

### State

Key-value storage with scoped prefixes:

```rust
pub trait State: Send + Sync {
    /// Get a value by key
    fn get(&self, key: &str) -> Option<Value>;
    
    /// Set a value
    fn set(&mut self, key: String, value: Value);
    
    /// Get all key-value pairs
    fn all(&self) -> HashMap<String, Value>;
}
```

**State Prefixes:**

State keys use prefixes to control scope and persistence:

| Prefix | Scope | Persistence | Use Case |
|--------|-------|-------------|----------|
| `user:` | User-level | Across all sessions | User preferences, settings |
| `app:` | Application-level | Application-wide | Shared configuration |
| `temp:` | Turn-level | Cleared each turn | Temporary computation data |
| (none) | Session-level | This session only | Conversation context |

```rust
// In a callback or tool
let state = ctx.session().state();

// User preference (persists across sessions)
state.set("user:theme".into(), json!("dark"));

// Session-specific data
state.set("current_topic".into(), json!("weather"));

// Temporary data (cleared after this turn)
state.set("temp:step_count".into(), json!(1));

// Read values
if let Some(theme) = state.get("user:theme") {
    println!("Theme: {}", theme);
}
```

---

## Error Handling

ADK uses a unified error type for all operations:

```rust
pub enum AdkError {
    Agent(String),      // Agent execution errors
    Tool(String),       // Tool execution errors
    Model(String),      // LLM API errors
    Session(String),    // Session storage errors
    Artifact(String),   // Artifact storage errors
    Config(String),     // Configuration errors
    Io(std::io::Error), // File/network I/O errors
    Json(serde_json::Error), // JSON parsing errors
}

pub type Result<T> = std::result::Result<T, AdkError>;
```

**Error Handling in Tools:**

```rust
async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let city = args["city"]
        .as_str()
        .ok_or_else(|| AdkError::Tool("Missing 'city' parameter".into()))?;
    
    let response = reqwest::get(&format!("https://api.weather.com/{}", city))
        .await
        .map_err(|e| AdkError::Tool(format!("API error: {}", e)))?;
    
    Ok(json!({ "weather": "sunny" }))
}
```

---

## EventStream

Agents return a stream of events rather than a single response:

```rust
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;
```

**Processing Events:**

```rust
use futures::StreamExt;

let mut stream = agent.run(ctx).await?;

while let Some(result) = stream.next().await {
    match result {
        Ok(event) => {
            // Check for text content
            if let Some(content) = event.content() {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        print!("{}", text);
                    }
                }
            }
            
            // Check for state changes
            for (key, value) in event.state_delta() {
                println!("State changed: {} = {}", key, value);
            }
            
            // Check if this is the final response
            if event.is_final_response() {
                println!("\n[Done]");
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            break;
        }
    }
}
```

---

## Llm Trait

The trait that all LLM providers implement:

```rust
#[async_trait]
pub trait Llm: Send + Sync {
    /// Model identifier (e.g., "gemini-2.0-flash", "gpt-4o")
    fn name(&self) -> &str;
    
    /// Generate content (streaming or non-streaming)
    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream>;
}
```

**LlmRequest:**

```rust
pub struct LlmRequest {
    pub contents: Vec<Content>,           // Conversation history
    pub tools: Vec<ToolDeclaration>,      // Available tools
    pub system_instruction: Option<String>, // System prompt
    pub config: GenerateContentConfig,    // Temperature, max_tokens, etc.
}
```

**LlmResponse:**

```rust
pub struct LlmResponse {
    pub content: Option<Content>,         // Generated content
    pub finish_reason: Option<FinishReason>, // Why generation stopped
    pub usage: Option<UsageMetadata>,     // Token counts
    pub partial: bool,                    // Is this a streaming chunk?
    pub turn_complete: bool,              // Is the turn finished?
}
```

All providers (Gemini, OpenAI, Anthropic, Ollama, etc.) implement this trait, making them interchangeable:

```rust
// Switch providers by changing one line
let model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&key, "gemini-2.0-flash")?);
// let model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(config)?);
// let model: Arc<dyn Llm> = Arc::new(AnthropicClient::new(config)?);

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .build()?;
```

---

**Previous**: [← Introduction](../introduction.md) | **Next**: [Runner →](runner.md)
