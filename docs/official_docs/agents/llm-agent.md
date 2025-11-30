# LlmAgent

The `LlmAgent` is the core agent type in ADK-Rust that uses a Large Language Model for reasoning and decision-making. It provides a flexible builder pattern for configuration and supports tools, sub-agents, callbacks, and instruction templating.

## Overview

An LlmAgent wraps an LLM (like Gemini) and provides:

- **Instruction templating** with session state variable injection
- **Tool integration** for extending agent capabilities
- **Sub-agent support** for building agent hierarchies
- **Callback hooks** for observing and modifying behavior
- **Output management** with schema validation and state storage

## Basic Usage

Create a minimal agent with just a name and model:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let agent = LlmAgentBuilder::new("my_agent")
        .model(Arc::new(model))
        .build()?;

    println!("Created agent: {}", agent.name());
    Ok(())
}
```

## Builder Methods

The `LlmAgentBuilder` provides a fluent API for configuring agents:

### Required Methods

| Method | Description |
|--------|-------------|
| `new(name)` | Creates a new builder with the agent's name |
| `model(model)` | Sets the LLM model (required) |
| `build()` | Builds the agent, returns `Result<LlmAgent>` |

### Configuration Methods

| Method | Description |
|--------|-------------|
| `description(desc)` | Human-readable description of the agent's purpose |
| `instruction(text)` | System instruction for the agent (supports templating) |
| `instruction_provider(fn)` | Dynamic instruction provider function |
| `global_instruction(text)` | Tree-wide instruction for agent hierarchies |
| `global_instruction_provider(fn)` | Dynamic global instruction provider |
| `include_contents(mode)` | Controls conversation history visibility |
| `output_key(key)` | Saves agent output to session state |
| `output_schema(schema)` | JSON schema for structured output |
| `input_schema(schema)` | JSON schema for input validation |

### Tool and Agent Methods

| Method | Description |
|--------|-------------|
| `tool(tool)` | Adds a tool to the agent |
| `sub_agent(agent)` | Adds a sub-agent for delegation |
| `disallow_transfer_to_parent(bool)` | Prevents transfer back to parent agent |
| `disallow_transfer_to_peers(bool)` | Prevents transfer to sibling agents |

### Callback Methods

| Method | Description |
|--------|-------------|
| `before_callback(fn)` | Called before agent execution |
| `after_callback(fn)` | Called after agent execution |
| `before_model_callback(fn)` | Called before LLM request |
| `after_model_callback(fn)` | Called after LLM response |
| `before_tool_callback(fn)` | Called before tool execution |
| `after_tool_callback(fn)` | Called after tool execution |

## Instruction Templating

Instructions support variable injection using `{var}` syntax. Variables are resolved from session state at runtime:

```rust
let agent = LlmAgentBuilder::new("greeter")
    .model(Arc::new(model))
    .instruction("You are helping {user_name}. Their preference is {preference}.")
    .build()?;
```

When the agent runs, `{user_name}` and `{preference}` are replaced with values from the session state. If a variable is not found, it remains as-is in the instruction.

### Setting State Variables

State variables can be set through:

1. **Session state** - Pre-populated when creating a session
2. **Tool responses** - Tools can update state via `EventActions`
3. **Output keys** - Agent output saved to state with `output_key()`

```rust
// Using output_key to save agent response to state
let summarizer = LlmAgentBuilder::new("summarizer")
    .model(Arc::new(model))
    .instruction("Summarize the following text concisely.")
    .output_key("summary")  // Response saved to state["summary"]
    .build()?;
```

## IncludeContents

The `IncludeContents` enum controls what conversation history the agent receives:

```rust
use adk_rust::prelude::*;

// Default - agent sees full conversation history
let agent = LlmAgentBuilder::new("agent")
    .model(Arc::new(model))
    .include_contents(IncludeContents::Default)
    .build()?;

// None - agent only sees current turn (stateless)
let stateless_agent = LlmAgentBuilder::new("stateless")
    .model(Arc::new(model))
    .include_contents(IncludeContents::None)
    .build()?;
```

### Options

| Value | Behavior |
|-------|----------|
| `IncludeContents::Default` | Agent receives full conversation history (default) |
| `IncludeContents::None` | Agent only sees current user input and instructions |

Use `None` for agents that should operate independently on each turn without context from previous interactions.

## Output Schema

For structured output, provide a JSON schema:

```rust
use serde_json::json;

let agent = LlmAgentBuilder::new("structured_agent")
    .model(Arc::new(model))
    .instruction("Extract the person's name and age from the text.")
    .output_schema(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "age": { "type": "integer" }
        },
        "required": ["name", "age"]
    }))
    .build()?;
```

The LLM will format its response according to the schema.

## Adding Tools

Tools extend agent capabilities with custom functions:

```rust
use adk_rust::prelude::*;
use serde_json::json;
use std::sync::Arc;

// Create a simple tool with FunctionTool::new(name, description, handler)
let weather_tool = FunctionTool::new(
    "get_weather",
    "Get the current weather for a location",
    |_ctx, args| async move {
        let location = args.get("location")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        Ok(json!({ "weather": "sunny", "location": location }))
    },
);

let agent = LlmAgentBuilder::new("weather_agent")
    .model(Arc::new(model))
    .instruction("You help users check the weather.")
    .tool(Arc::new(weather_tool))
    .build()?;
```

## Dynamic Instructions

For instructions that need runtime computation, use an instruction provider:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("dynamic_agent")
    .model(Arc::new(model))
    .instruction_provider(|ctx| {
        Box::pin(async move {
            let user_id = ctx.user_id();
            Ok(format!("You are assisting user {}. Be helpful and concise.", user_id))
        })
    })
    .build()?;
```

The provider receives a `ReadonlyContext` with access to session information.

## Complete Example

Here's a fully configured agent demonstrating multiple features:

```rust
use adk_rust::prelude::*;
use adk_rust::IncludeContents;
use std::sync::Arc;
use serde_json::json;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create a tool with FunctionTool::new(name, description, handler)
    let calculator = FunctionTool::new(
        "calculate",
        "Perform basic arithmetic",
        |_ctx, args| async move {
            let expr = args.get("expression")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            // Simple evaluation (in production, use a proper parser)
            Ok(json!({ "result": expr, "note": "Expression received" }))
        },
    );

    // Build the agent with full configuration
    let agent = LlmAgentBuilder::new("math_assistant")
        .description("A helpful math assistant that can perform calculations")
        .instruction("You are a math tutor helping {user_name}. \
                     Use the calculator tool for arithmetic operations. \
                     Explain your reasoning step by step.")
        .model(Arc::new(model))
        .tool(Arc::new(calculator))
        .include_contents(IncludeContents::Default)
        .output_key("last_response")
        .build()?;

    println!("Created agent: {}", agent.name());
    println!("Description: {}", agent.description());
    
    Ok(())
}
```

## API Reference

See the rustdoc for `LlmAgentBuilder` for complete API documentation.

## Related

- [Workflow Agents](workflow-agents.md) - Sequential, Parallel, and Loop agents
- [Multi-Agent Systems](multi-agent.md) - Building agent hierarchies
- [Function Tools](../tools/function-tools.md) - Creating custom tools
- [Callbacks](../callbacks/callbacks.md) - Intercepting agent behavior
