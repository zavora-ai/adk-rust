# Quickstart

This guide shows you how to get up and running with ADK-Rust. You'll create your first AI agent in under 10 minutes.

## Prerequisites

Before you start, make sure you have:

- Rust 1.75 or later (`rustup update stable`)
- A Google API key for Gemini

## Step 1: Create a New Project

Create a new Rust project:

```bash
cargo new my_agent
cd my_agent
```

Your project structure will look like this:

```
my_agent/
├── Cargo.toml
├── src/
│   └── main.rs
└── .env          # You'll create this for your API key
```

## Step 2: Add Dependencies

Update your `Cargo.toml` with the required dependencies:

```toml
[package]
name = "my_agent"
version = "0.1.0"
edition = "2021"

[dependencies]
adk-rust = "0.1"
tokio = { version = "1.40", features = ["full"] }
dotenv = "0.15"
```

Install the dependencies:

```bash
cargo build
```

## Step 3: Set Up Your API Key

This project uses the Gemini API, which requires an API key. If you don't have one, create a key in [Google AI Studio](https://aistudio.google.com/app/apikey).

Create a `.env` file in your project root:

**Linux / macOS:**

```bash
echo 'GOOGLE_API_KEY=your-api-key-here' > .env
```

**Windows (PowerShell):**

```powershell
echo GOOGLE_API_KEY=your-api-key-here > .env
```

> **Security Tip:** Add `.env` to your `.gitignore` to avoid committing your API key.

## Step 4: Write Your Agent

Replace the contents of `src/main.rs` with:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables from .env file
    dotenv::dotenv().ok();
    
    // Get API key from environment
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Build your agent
    let agent = LlmAgentBuilder::new("my_assistant")
        .description("A helpful AI assistant")
        .instruction("You are a friendly and helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .build()?;

    // Run the agent with the CLI launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

## Step 5: Run Your Agent

Start your agent in interactive console mode:

```bash
cargo run
```

You'll see a prompt where you can chat with your agent:

```
🤖 Agent ready! Type your questions (or 'exit' to quit).

You: Hello! What can you help me with?
Assistant: Hello! I'm a helpful AI assistant. I can help you with:
- Answering questions on various topics
- Explaining concepts
- Providing information and suggestions
- Having a friendly conversation

What would you like to know?

You: exit
👋 Goodbye!
```

## Step 6: Add a Tool (Optional)

Let's enhance your agent with the Google Search tool to give it access to real-time information:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Build agent with Google Search tool
    let agent = LlmAgentBuilder::new("search_assistant")
        .description("An assistant that can search the web")
        .instruction("You are a helpful assistant. Use the search tool to find current information when needed.")
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool::new()))  // Add search capability
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

Now your agent can search the web:

```
You: What's the weather like in Tokyo today?
Assistant: Let me search for that information...
[Using GoogleSearchTool]
Based on current information, Tokyo is experiencing...
```

## Running as a Web Server

For a web-based interface, run with the `serve` command:

```bash
cargo run -- serve
```

Or specify a custom port:

```bash
cargo run -- serve --port 3000
```

Access the web interface at [http://localhost:8080](http://localhost:8080).

## Understanding the Code

Let's break down what each part does:

### Model Creation

```rust
let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
```

Creates a Gemini model instance. The model handles all LLM interactions.

### Agent Building

```rust
let agent = LlmAgentBuilder::new("my_assistant")
    .description("A helpful AI assistant")
    .instruction("You are a friendly assistant...")
    .model(Arc::new(model))
    .build()?;
```

- `new("name")`: Sets the agent's identifier
- `description()`: Brief description of what the agent does
- `instruction()`: System prompt that guides the agent's behavior
- `model()`: The LLM to use for reasoning
- `build()`: Creates the agent instance

### Launcher

```rust
Launcher::new(Arc::new(agent)).run().await?;
```

The Launcher provides CLI support for running agents in console or server mode.

## Using Other Models

ADK-Rust is model-agnostic. You can implement the `Llm` trait to use other providers:

```rust
use adk_rust::prelude::*;

pub struct MyCustomModel {
    // Your model configuration
}

#[async_trait]
impl Llm for MyCustomModel {
    async fn generate(&self, request: LlmRequest) -> Result<LlmResponse> {
        // Your implementation
    }
    
    async fn generate_stream(&self, request: LlmRequest) -> Result<LlmResponseStream> {
        // Your streaming implementation
    }
}
```

## Next Steps

Now that you have your first agent running, explore these topics:

- [LlmAgent Configuration](agents/llm-agent.md) - All configuration options
- [Function Tools](tools/function-tools.md) - Create custom tools
- [Workflow Agents](agents/workflow-agents.md) - Build multi-step pipelines
- [Sessions](sessions/sessions.md) - Manage conversation state
- [Callbacks](callbacks/callbacks.md) - Customize agent behavior

---

**Previous**: [Introduction](introduction.md) | **Next**: [LlmAgent](agents/llm-agent.md)
