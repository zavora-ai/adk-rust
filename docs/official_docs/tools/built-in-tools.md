# Built-in Tools

ADK-Rust provides several built-in tools that extend agent capabilities without requiring custom implementation. These tools are ready to use out of the box and integrate seamlessly with the agent framework.

## Overview

| Tool | Purpose | Use Case |
|------|---------|----------|
| `GoogleSearchTool` | Web search via Gemini | Real-time information retrieval |
| `ExitLoopTool` | Loop termination | Controlling LoopAgent iterations |
| `LoadArtifactsTool` | Artifact loading | Accessing stored binary data |

## GoogleSearchTool

`GoogleSearchTool` enables agents to search the web using Google Search. This tool is handled internally by Gemini models through the grounding feature, meaning the search is performed server-side by the model itself.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create the GoogleSearchTool
    let search_tool = GoogleSearchTool;

    // Add to agent
    let agent = LlmAgentBuilder::new("research_assistant")
        .description("An assistant that can search the web for information")
        .instruction(
            "You are a research assistant. When asked about current events, \
             recent news, or factual information, use the google_search tool \
             to find accurate, up-to-date information."
        )
        .model(Arc::new(model))
        .tool(Arc::new(search_tool))
        .build()?;

    println!("Agent created with Google Search capability!");
    Ok(())
}
```

### How It Works

Unlike regular function tools, `GoogleSearchTool` operates differently:

1. **Server-side execution**: The search is performed by Gemini's grounding feature, not locally
2. **Automatic invocation**: The model decides when to search based on the query
3. **Integrated results**: Search results are incorporated directly into the model's response

The tool implementation returns an error if called directly because the actual search happens within the Gemini API:

```rust
// This is handled internally - you don't call it directly
async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
    Err(AdkError::Tool("GoogleSearch is handled internally by Gemini".to_string()))
}
```

### Tool Details

| Property | Value |
|----------|-------|
| Name | `google_search` |
| Description | "Performs a Google search to retrieve information from the web." |
| Parameters | Determined by Gemini model |
| Execution | Server-side (Gemini grounding) |

### Use Cases

- **Current events**: "What happened in the news today?"
- **Factual queries**: "What is the population of Tokyo?"
- **Recent information**: "What are the latest developments in AI?"
- **Research tasks**: "Find information about renewable energy trends"

### Example Queries

```rust
// The agent will automatically use Google Search for queries like:
// - "What's the weather forecast for New York this week?"
// - "Who won the latest championship game?"
// - "What are the current stock prices for tech companies?"
```

## ExitLoopTool

`ExitLoopTool` is a control tool used with `LoopAgent` to signal when an iterative process should terminate. When called, it sets the `escalate` flag, causing the loop to exit.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create an agent with ExitLoopTool for iterative refinement
    let refiner = LlmAgentBuilder::new("content_refiner")
        .description("Iteratively improves content quality")
        .instruction(
            "Review the content and improve it. Check for:\n\
             1. Clarity and readability\n\
             2. Grammar and spelling\n\
             3. Logical flow\n\n\
             If the content meets all quality standards, call the exit_loop tool.\n\
             Otherwise, provide an improved version."
        )
        .model(Arc::new(model))
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    // Use in a LoopAgent
    let loop_agent = LoopAgent::new(
        "iterative_refiner",
        vec![Arc::new(refiner)],
    ).with_max_iterations(5);

    println!("Loop agent created with exit capability!");
    Ok(())
}
```

### How It Works

1. The agent evaluates whether to continue or exit
2. When ready to exit, the agent calls `exit_loop`
3. The tool sets `actions.escalate = true` and `actions.skip_summarization = true`
4. The `LoopAgent` detects the escalate flag and stops iterating

### Tool Details

| Property | Value |
|----------|-------|
| Name | `exit_loop` |
| Description | "Exits the loop. Call this function only when you are instructed to do so." |
| Parameters | None |
| Returns | Empty object `{}` |

### Best Practices

1. **Clear exit criteria**: Define specific conditions in the agent's instruction
2. **Always set max_iterations**: Prevent infinite loops as a safety measure
3. **Meaningful instructions**: Help the agent understand when to exit

```rust
// Good: Clear exit criteria
.instruction(
    "Improve the text until it:\n\
     - Has no grammatical errors\n\
     - Is under 100 words\n\
     - Uses active voice\n\
     When all criteria are met, call exit_loop."
)

// Avoid: Vague criteria
.instruction("Improve the text. Exit when done.")
```

## LoadArtifactsTool

`LoadArtifactsTool` allows agents to retrieve stored artifacts by name. This is useful when agents need to access files, images, or other binary data that was previously saved.

### Basic Usage

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with artifact loading capability
    let agent = LlmAgentBuilder::new("document_analyzer")
        .description("Analyzes stored documents")
        .instruction(
            "You can load and analyze stored artifacts. \
             Use the load_artifacts tool to retrieve documents by name. \
             The tool accepts an array of artifact names."
        )
        .model(Arc::new(model))
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()?;

    println!("Agent created with artifact loading capability!");
    Ok(())
}
```

### Tool Details

| Property | Value |
|----------|-------|
| Name | `load_artifacts` |
| Description | "Loads artifacts by name and returns their content. Accepts an array of artifact names." |
| Parameters | `artifact_names`: Array of strings |
| Returns | Object with `artifacts` array |

### Parameters

The tool expects a JSON object with an `artifact_names` array:

```json
{
  "artifact_names": ["document.txt", "image.png", "data.json"]
}
```

### Response Format

The tool returns an object containing the loaded artifacts:

```json
{
  "artifacts": [
    {
      "name": "document.txt",
      "content": "The text content of the document..."
    },
    {
      "name": "image.png",
      "content": {
        "mime_type": "image/png",
        "data": "base64-encoded-data..."
      }
    },
    {
      "name": "missing.txt",
      "error": "Artifact not found"
    }
  ]
}
```

### Requirements

For `LoadArtifactsTool` to work, you need:

1. An `ArtifactService` configured in the runner
2. Artifacts previously saved to the service
3. The tool added to the agent

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

// Set up artifact service
let artifact_service = Arc::new(InMemoryArtifactService::new());

// Configure runner with artifact service
let runner = Runner::new(agent)
    .with_artifact_service(artifact_service);
```

## Combining Built-in Tools

You can use multiple built-in tools together:

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create agent with multiple built-in tools
    let agent = LlmAgentBuilder::new("research_agent")
        .description("Research agent with search and artifact capabilities")
        .instruction(
            "You are a research agent. You can:\n\
             - Search the web using google_search for current information\n\
             - Load stored documents using load_artifacts\n\
             Use these tools to help answer questions comprehensively."
        )
        .model(Arc::new(model))
        .tool(Arc::new(GoogleSearchTool))
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()?;

    println!("Multi-tool agent created!");
    Ok(())
}
```

## Creating Custom Built-in Tools

You can create your own tools following the same pattern as built-in tools by implementing the `Tool` trait:

```rust
use adk_rust::prelude::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct MyCustomTool;

impl MyCustomTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for MyCustomTool {
    fn name(&self) -> &str {
        "my_custom_tool"
    }

    fn description(&self) -> &str {
        "Description of what this tool does"
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // Your tool logic here
        Ok(json!({ "result": "success" }))
    }
}
```

## API Reference

### GoogleSearchTool

```rust
impl GoogleSearchTool {
    /// Create a new GoogleSearchTool instance
    pub fn new() -> Self;
}
```

### ExitLoopTool

```rust
impl ExitLoopTool {
    /// Create a new ExitLoopTool instance
    pub fn new() -> Self;
}
```

### LoadArtifactsTool

```rust
impl LoadArtifactsTool {
    /// Create a new LoadArtifactsTool instance
    pub fn new() -> Self;
}

impl Default for LoadArtifactsTool {
    fn default() -> Self;
}
```

## Related

- [Function Tools](function-tools.md) - Creating custom function tools
- [MCP Tools](mcp-tools.md) - Using MCP servers as tool providers
- [Workflow Agents](../agents/workflow-agents.md) - Using ExitLoopTool with LoopAgent
- [Artifacts](../artifacts/artifacts.md) - Managing binary data with artifacts
