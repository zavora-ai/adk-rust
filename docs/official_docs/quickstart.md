# Quickstart

This guide shows you how to get up and running with ADK-Rust. You'll create your first AI agent in under 10 minutes.

## Prerequisites

Before you start, make sure you have:

- Rust 1.85.0 or later (`rustup update stable`)
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
edition = "2024"

[dependencies]
adk-rust = "0.3.2"
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
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
    dotenvy::dotenv().ok();
    
    // Get API key from environment
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

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

## Step 6: Add a Tool

Let's enhance your agent with the Google Search tool to give it access to real-time information:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")
        .expect("GOOGLE_API_KEY environment variable not set");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

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

Start your agent again in interactive console mode:

```bash
cargo run
```

Now you can prompt your agent to search the web:

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

This starts the server on the **default port 8080**. Access it at [http://localhost:8080](http://localhost:8080).

To specify a custom port:

```bash
cargo run -- serve --port 3000
```

This starts the server on **port 3000**. Access it at [http://localhost:3000](http://localhost:3000).

## Understanding the Code

Let's break down what each part does:

### Imports

```rust
use adk_rust::prelude::*;  // GeminiModel, LlmAgentBuilder, Arc, etc.
use adk_rust::Launcher;    // CLI launcher for console/server modes
use std::sync::Arc;        // Thread-safe reference counting pointer
```

- **`prelude::*`** imports all commonly used types: `GeminiModel`, `LlmAgentBuilder`, `Arc`, error types, and more
- **`Launcher`** provides the CLI interface for running agents
- **`Arc`** (Atomic Reference Counted) enables safe sharing of the model and agent across async tasks

### Model Creation

```rust
let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;
```

Creates a Gemini model instance that implements the `Llm` trait. The model:
- Handles authentication with your API key
- Manages streaming responses from the LLM
- Supports function calling for tools

### Agent Building

```rust
let agent = LlmAgentBuilder::new("my_assistant")
    .description("A helpful AI assistant")
    .instruction("You are a friendly assistant...")
    .model(Arc::new(model))
    .build()?;
```

The builder pattern configures your agent:

| Method | Purpose |
|--------|--------|
| `new("name")` | Sets the agent's unique identifier (used in logs and multi-agent systems) |
| `description()` | Brief description shown in agent cards and A2A protocol |
| `instruction()` | **System prompt** - defines the agent's personality and behavior |
| `model(Arc::new(...))` | Wraps the model in `Arc` for thread-safe sharing |
| `tool(Arc::new(...))` | *(Optional)* Adds tools/functions the agent can call |
| `build()` | Validates configuration and creates the agent instance |

### Launcher

```rust
Launcher::new(Arc::new(agent)).run().await?;
```

The Launcher handles the runtime:
- **Console mode** (default): Interactive chat in your terminal
- **Server mode** (`-- serve`): REST API with web interface
- Manages session state, streaming responses, and graceful shutdown

## Using Other Models

ADK-Rust supports multiple LLM providers out of the box. Enable them via feature flags in your `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "0.3.2", features = ["openai", "anthropic", "deepseek", "groq", "ollama"] }
```

Set the appropriate API key for your provider:

```bash
# OpenAI
export OPENAI_API_KEY="your-api-key"

# Anthropic
export ANTHROPIC_API_KEY="your-api-key"

# DeepSeek
export DEEPSEEK_API_KEY="your-api-key"

# Groq
export GROQ_API_KEY="your-api-key"

# Ollama (no key needed, just run: ollama serve)
```

### OpenAI

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

### Anthropic

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

### DeepSeek

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY")?;

    // Standard chat model
    let model = DeepSeekClient::chat(api_key)?;

    // Or use reasoner for chain-of-thought reasoning
    // let model = DeepSeekClient::reasoner(api_key)?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

### Groq (Ultra-Fast Inference)

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GROQ_API_KEY")?;
    let model = GroqClient::new(GroqConfig::llama70b(api_key))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

### Ollama (Local Models)

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    // Requires: ollama serve && ollama pull llama3.2
    let model = OllamaModel::new(OllamaConfig::new("llama3.2"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
```

### Supported Models

| Provider | Model Examples | Feature Flag |
|----------|---------------|--------------|
| Gemini | `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-3-pro-preview`, `gemini-3-flash-preview` | (default) |
| OpenAI | `gpt-5`, `gpt-5-mini`, `gpt-5-nano` | `openai` |
| Anthropic | `claude-sonnet-4-5-20250929`, `claude-opus-4-5-20251101`, `claude-haiku-4-5-20251001` | `anthropic` |
| DeepSeek | `deepseek-chat`, `deepseek-reasoner` | `deepseek` |
| Groq | `meta-llama/llama-4-scout-17b-16e-instruct`, `llama-3.3-70b-versatile`, `llama-3.1-8b-instant` | `groq` |
| Ollama | `llama3.2:3b`, `qwen2.5:7b`, `mistral:7b`, `deepseek-r1:14b`, `gemma3:9b` | `ollama` |

## Next Steps

Now that you have your first agent running, explore these topics:

- [LlmAgent Configuration](agents/llm-agent.md) - All configuration options
- [Function Tools](tools/function-tools.md) - Create custom tools
- [Workflow Agents](agents/workflow-agents.md) - Build multi-step pipelines
- [Sessions](sessions/sessions.md) - Manage conversation state
- [Callbacks](callbacks/callbacks.md) - Customize agent behavior

---

**Previous**: [Introduction](introduction.md) | **Next**: [LlmAgent](agents/llm-agent.md)
