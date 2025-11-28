# Quick Start Guide

Build your first ADK-Rust agent in minutes. This guide walks you through creating a simple weather agent that can answer questions using Google Search.

## Prerequisites

- Rust 1.75+ installed
- `GOOGLE_API_KEY` environment variable set
- Basic familiarity with Rust and async/await

See [Installation](02_installation.md) if you haven't set up yet.

## Step 1: Create a New Project

```bash
cargo new weather-agent
cd weather-agent
```

## Step 2: Add Dependencies

Edit `Cargo.toml`:

```toml
[package]
name = "weather-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
# ADK crates
adk-core = "0.1"
adk-agent = "0.1"
adk-model = "0.1"
adk-tool = "0.1"
adk-runner = "0.1"
adk-session = "0.1"

# Async runtime
tokio = { version = "1.40", features = ["full"] }

# Utilities
anyhow = "1.0"
```

## Step 3: Write Your Agent

Replace `src/main.rs` with:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Part};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use adk_tool::GoogleSearchTool;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load API key
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // 2. Create Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // 3. Build agent with Google Search tool
    let agent = LlmAgentBuilder::new("weather-agent")
        .description("A helpful assistant that can search for weather information")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))
        .build()?;

    // 4. Create runner with in-memory session
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new("weather-app", Arc::new(agent), session_service);

    // 5. Run a query
    let user_id = "user123".to_string();
    let session_id = "session456".to_string();
    let query = Content::text("What's the weather like in San Francisco today?");

    println!("🤖 Processing: {}", query.parts[0].text().unwrap_or(""));
    
    // 6. Stream events
    let mut events = runner.run(user_id, session_id, query).await?;
    
    use futures::StreamExt;
    while let Some(event) = events.next().await {
        match event {
            Ok(evt) => {
                if let Some(content) = evt.content {
                    for part in content.parts {
                        if let Some(text) = part.text() {
                            println!("📝 {}", text);
                        }
                    }
                }
            }
            Err(e) => eprintln!("❌ Error: {}", e),
        }
    }

    Ok(())
}
```

## Step 4: Run Your Agent

```bash
# Set API key (if not already set)
export GOOGLE_API_KEY="your-api-key-here"

# Run the agent
cargo run
```

**Expected Output**:
```
🤖 Processing: What's the weather like in San Francisco today?
📝 Searching for current weather in San Francisco...
📝 According to recent reports, San Francisco has partly cloudy skies 
   with temperatures around 62°F (17°C). Expect mild conditions with 
   light winds from the west.
```

## Understanding the Code

Let's break down what each part does:

### 1. Load API Key
```rust
let api_key = std::env::var("GOOGLE_API_KEY")
    .expect("GOOGLE_API_KEY environment variable not set");
```
Retrieves your Google AI API key from the environment.

### 2. Create the Model
```rust
let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
```
Initializes the Gemini 2.0 Flash model for generating responses.

### 3. Build the Agent
```rust
let agent = LlmAgentBuilder::new("weather-agent")
    .description("A helpful assistant...")
    .model(Arc::new(model))
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;
```
Creates an LLM-powered agent with:
- A unique name
- A description (used in multi-agent scenarios)
- The Gemini model
- Google Search tool for real-time information

### 4. Create the Runner
```rust
let session_service = Arc::new(InMemorySessionService::new());
let runner = Runner::new("weather-app", Arc::new(agent), session_service);
```
Sets up the execution runtime with in-memory session storage.

### 5. Process a Query
```rust
let query = Content::text("What's the weather like in San Francisco today?");
let mut events = runner.run(user_id, session_id, query).await?;
```
Sends user input to the agent and gets back an event stream.

### 6. Handle Events
```rust
while let Some(event) = events.next().await {
    // Process each event (partial responses, tool calls, etc.)
}
```
Iterates over the stream to receive real-time updates.

## Next Steps

### Make It Interactive

Want a chat interface? Try the interactive console:

```bash
# Run the quickstart example with console mode
cargo run --example quickstart
```

Or add this to your code:

```rust
use rustyline::DefaultEditor;

let mut rl = DefaultEditor::new()?;
loop {
    let input = rl.readline("You: ")?;
    if input.trim() == "exit" { break; }
    
    let query = Content::text(&input);
    let mut events = runner.run(
        user_id.clone(),
        session_id.clone(),
        query
    ).await?;
    
    // Process events...
}
```

### Add More Tools

Extend your agent with custom tools:

```rust
use adk_tool::FunctionTool;

let calculator = FunctionTool::new(
    "calculate",
    "Perform arithmetic calculations",
    |_ctx, args| async move {
        // Your calculation logic here
        Ok(serde_json::json!({"result": 42}))
    }
);

let agent = LlmAgentBuilder::new("math-agent")
    .model(Arc::new(model))
    .tool(Arc::new(calculator))
    .build()?;
```

### Persist Conversations

Use database storage instead of in-memory:

```rust
use adk_session::DatabaseSessionService;

let session_service = Arc::new(
    DatabaseSessionService::new("sqlite://sessions.db").await?
);
let runner = Runner::new("app", Arc::new(agent), session_service);
```

### Build Workflows

Chain multiple agents together:

```rust
use adk_agent::SequentialAgent;

let analyzer = LlmAgentBuilder::new("analyzer")
    .model(Arc::new(model.clone()))
    .instruction("Analyze the user query")
    .build()?;

let researcher = LlmAgentBuilder::new("researcher")
    .model(Arc::new(model.clone()))
    .tool(Arc::new(GoogleSearchTool::new()))
    .build()?;

let summarizer = LlmAgentBuilder::new("summarizer")
    .model(Arc::new(model))
    .instruction("Summarize the findings")
    .build()?;

let workflow = SequentialAgent::new(
    "research-workflow",
    vec![
        Arc::new(analyzer),
        Arc::new(researcher),
        Arc::new(summarizer),
    ],
);
```

### Deploy as a Server

Run your agent as an HTTP API:

```rust
use adk_server::ServerConfig;

let config = ServerConfig {
    host: "0.0.0.0".to_string(),
    port: 8080,
    app_name: "weather-api".to_string(),
};

adk_server::start_server(config, Arc::new(agent), session_service).await?;
```

Then call it:
```bash
curl -X POST http://localhost:8080/api/run \
  -H "Content-Type: application/json" \
  -d '{
    "userId": "user123",
    "sessionId": "session456",
    "message": "What is the weather in Tokyo?"
  }'
```

## Learn More

### Explore Examples

ADK-Rust includes 12 examples covering different use cases:

```bash
# See all examples
ls examples/

# Run any example
cargo run --example function_tool
cargo run --example sequential
cargo run --example server
```

See the [Examples README](../examples/README.md) for details.

### Dive Deeper

- **[Core Concepts](04_concepts.md)**: Understand agents, models, tools, and sessions
- **[API Reference](05_api_reference.md)**: Detailed API documentation
- **[Workflow Patterns](07_workflows.md)**: Build complex agent orchestrations
- **[Deployment Guide](08_deployment.md)**: Deploy to production

## Troubleshooting

### "API key not found"

Make sure you've set the environment variable:
```bash
export GOOGLE_API_KEY="your-key-here"
```

### "Model error: 401 Unauthorized"

Your API key may be invalid. Get a new key from [Google AI Studio](https://aistudio.google.com/app/apikey).

### "Tool not found" or "No response"

The agent may not have the right tools. Ensure you've added tools:
```rust
.tool(Arc::new(GoogleSearchTool::new()))
```

### Async Runtime Error

Make sure your `main` function uses `#[tokio::main]`:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // ...
}
```

---

**Previous**: [Installation](02_installation.md) | **Next**: [Core Concepts](04_concepts.md)
