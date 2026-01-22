# LlmAgent

The `LlmAgent` is the core agent type in ADK-Rust that uses a Large Language Model for reasoning and decision-making.

## Quick Start

Create a new project:

```bash
cargo new llm_agent
cd llm_agent
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-rust = "0.2.1"
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
serde_json = "1.0"
```

Create `.env` with your API key:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

Replace `src/main.rs`:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("my_agent")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

Run it:

```bash
cargo run
```

### Interacting with Your Agent

You'll see an interactive prompt:

```
🤖 Agent ready! Type your questions (or 'exit' to quit).

You: Hello! What can you help me with?
Assistant: Hello! I'm a helpful assistant. I can help you with:
- Answering questions on various topics
- Explaining concepts clearly
- Having a conversation

What would you like to know?

You: exit
👋 Goodbye!
```

---

## Shaping Agent Behavior with Instructions

The `instruction()` method defines your agent's personality and behavior. This is the **system prompt** that guides every response:

```rust
// A formal business assistant
let formal_agent = LlmAgentBuilder::new("formal_assistant")
    .instruction("You are a professional business consultant. \
                 Use formal language. Be concise and data-driven.")
    .model(Arc::new(model))
    .build()?;

// A friendly coding tutor
let tutor_agent = LlmAgentBuilder::new("code_tutor")
    .instruction("You are a friendly coding tutor for beginners. \
                 Explain concepts simply. Use examples. \
                 Encourage questions. Never make the user feel bad for not knowing.")
    .model(Arc::new(model))
    .build()?;

// A creative storyteller
let story_agent = LlmAgentBuilder::new("storyteller")
    .instruction("You are a creative storyteller. \
                 Craft engaging narratives with vivid descriptions. \
                 Use plot twists and memorable characters.")
    .model(Arc::new(model))
    .build()?;
```

### Example Outputs

**User prompt:** "What is Rust?"

**Formal Business Assistant:**
```
Rust is a systems programming language focused on performance, memory safety, 
and concurrency. Key metrics: 0 runtime overhead, 0 garbage collection. 
Adoption growing 30% YoY in enterprise. Recommended for performance-critical 
applications where reliability is paramount.
```

**Friendly Coding Tutor:**
```
Great question! 🎉 Rust is a programming language that helps you write fast 
and safe code. Think of it like a helpful friend who stops you from making 
mistakes before they happen!

Here's a simple example:
  let name = "Alice";  // This creates a variable

What makes Rust special is that it catches bugs at compile time, not when 
your program is running. Pretty cool, right? Want me to explain more?
```

**Creative Storyteller:**
```
In the twilight of 2010, in a secret Mozilla laboratory, a lone developer 
named Graydon Hoare breathed life into something extraordinary—a language 
that would challenge the old gods of C and C++.

They called it Rust, named for a fungus that survives against all odds. 
Like a phoenix rising from the ashes of segmentation faults and memory 
leaks, Rust promised a new era: speed without sacrifice, power without peril.

And so, the revolution began...
```

### Instruction Templating

Instructions support variable injection using `{var}` syntax. Variables are resolved from session state at runtime:

```rust
let agent = LlmAgentBuilder::new("personalized")
    .instruction("You are helping {user_name}. Their role is {user_role}. \
                 Tailor your responses to their expertise level.")
    .model(Arc::new(model))
    .build()?;
```

**Step-by-step guide to use templating:**

1. **Create the agent** with template variables in the instruction
2. **Set up Runner and SessionService** to manage state
3. **Create session with state variables** that match your template
4. **Run the agent** - templates get replaced automatically

Here's a complete working example:

```rust
use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, InMemorySessionService, SessionService};
use adk_rust::futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // 1. Agent with templated instruction
    let agent = LlmAgentBuilder::new("personalized")
        .instruction("You are helping {user_name}. Their role is {user_role}. \
                     Tailor your responses to their expertise level.")
        .model(Arc::new(model))
        .build()?;

    // 2. Create session service and runner
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(RunnerConfig {
        app_name: "templating_demo".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        run_config: None,
    })?;

    // 3. Create session with state variables
    let mut state = HashMap::new();
    state.insert("user_name".to_string(), json!("Alice"));
    state.insert("user_role".to_string(), json!("Senior Developer"));

    let session = session_service.create(CreateRequest {
        app_name: "templating_demo".to_string(),
        user_id: "user123".to_string(),
        session_id: None,
        state,
    }).await?;

    // 4. Run the agent - instruction becomes:
    // "You are helping Alice. Their role is Senior Developer..."
    let mut response_stream = runner.run(
        "user123".to_string(),
        session.id().to_string(),
        Content::new("user").with_text("Explain async/await in Rust"),
    ).await?;

    // Print the response
    while let Some(event) = response_stream.next().await {
        let event = event?;
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{}", text);
                }
            }
        }
    }

    Ok(())
}
```

**Template Variable Types:**

| Pattern | Example | Source |
|---------|---------|--------|
| `{var}` | `{user_name}` | Session state |
| `{prefix:var}` | `{user:name}`, `{app:config}` | Prefixed state |
| `{var?}` | `{user_name?}` | Optional (empty if missing) |
| `{artifact.file}` | `{artifact.resume.pdf}` | Artifact content |

**Output Example:**

Template: `"You are helping {user_name}. Their role is {user_role}."`  
Becomes: `"You are helping Alice. Their role is Senior Developer."`

The agent will then respond with personalized content based on the user's name and expertise level!

---

## Adding Tools

Tools give your agent abilities beyond conversation—they can fetch data, perform calculations, search the web, or call external APIs. The LLM decides when to use a tool based on the user's request.

### How Tools Work

1. **Agent receives user message** → "What's the weather in Tokyo?"
2. **LLM decides to call tool** → Selects `get_weather` with `{"city": "Tokyo"}`
3. **Tool executes** → Returns `{"temperature": "22°C", "condition": "sunny"}`
4. **LLM formats response** → "The weather in Tokyo is sunny at 22°C."

### Creating a Tool with FunctionTool

`FunctionTool` is the simplest way to create a tool—wrap any async Rust function and the LLM can call it. You provide a name, description, and handler function that receives JSON arguments and returns a JSON result.

```rust
let weather_tool = FunctionTool::new(
    "get_weather",                              // Tool name (used by LLM)
    "Get the current weather for a city",       // Description (helps LLM decide when to use it)
    |_ctx, args| async move {                   // Handler function
        let city = args.get("city")             // Extract arguments from JSON
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        Ok(json!({ "city": city, "temperature": "22°C" }))  // Return JSON result
    },
);
```
> **⚠️ Note: Current Limitation**: Built-in tools like `GoogleSearchTool` are currently incompatible with `FunctionTool` in the same agent. Use either built-in tools OR custom `FunctionTool`s, but not both together. 
**💡 Workaround**: Create separate subagents, each with their own tool type, and coordinate them using a master LLMAgent, workflow agents or multi-agent patterns.

### Build a Multi-Tool Agent

Create a new project:

```bash
cargo new tool_agent
cd tool_agent
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "0.2.1", features = ["tools"] }
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
serde_json = "1.0"
```

Create `.env`:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

Replace `src/main.rs` with an agent that has three tools:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Tool 1: Weather lookup
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get the current weather for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({ "city": city, "temperature": "22°C", "condition": "sunny" }))
        },
    );

    // Tool 2: Calculator
    let calculator = FunctionTool::new(
        "calculate",
        "Perform arithmetic. Parameters: a (number), b (number), operation (add/subtract/multiply/divide)",
        |_ctx, args| async move {
            let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let op = args.get("operation").and_then(|v| v.as_str()).unwrap_or("add");
            let result = match op {
                "add" => a + b,
                "subtract" => a - b,
                "multiply" => a * b,
                "divide" => if b != 0.0 { a / b } else { 0.0 },
                _ => 0.0,
            };
            Ok(json!({ "result": result }))
        },
    );

    // Tool 3: Built-in Google Search (Note: Currently unsupported in ADK-Rust)
    // let search_tool = GoogleSearchTool::new();

    // Build agent with weather and calculator tools
    let agent = LlmAgentBuilder::new("multi_tool_agent")
        .instruction("You are a helpful assistant. Use tools when needed: \
                     - get_weather for weather questions \
                     - calculate for math")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .tool(Arc::new(calculator))
        // .tool(Arc::new(search_tool))  // Currently unsupported
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

Run your agent:

```bash
cargo run
```

### Example Interaction

```
You: What's 15% of 250?
Assistant: [Using calculate tool with a=250, b=0.15, operation=multiply]
15% of 250 is 37.5.

You: What's the weather in Tokyo?
Assistant: [Using get_weather tool with city=Tokyo]
The weather in Tokyo is sunny with a temperature of 22°C.

You: Search for latest Rust features
Assistant: I don't have access to search functionality at the moment, but I can help with other questions about Rust or perform calculations!
```

---

## Structured Output with JSON Schema

For applications that need structured data, use `output_schema()`:

```rust
use adk_rust::prelude::*;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let extractor = LlmAgentBuilder::new("entity_extractor")
        .instruction("Extract entities from the given text.")
        .model(Arc::new(model))
        .output_schema(json!({
            "type": "object",
            "properties": {
                "people": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "locations": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "dates": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["people", "locations", "dates"]
        }))
        .build()?;

    println!("Entity extractor ready!");
    Ok(())
}
```

### JSON Output Example

Input: "John met Sarah in Paris on December 25th"

Output:
```json
{
  "people": ["John", "Sarah"],
  "locations": ["Paris"],
  "dates": ["December 25th"]
}
```

---

## Advanced Features

### Include Contents

Control conversation history visibility:

```rust
// Full history (default)
.include_contents(IncludeContents::Default)

// Stateless - only sees current input
.include_contents(IncludeContents::None)
```

### Output Key

Save agent responses to session state:

```rust
.output_key("summary")  // Response saved to state["summary"]
```

### Dynamic Instructions

Compute instructions at runtime:

```rust
.instruction_provider(|ctx| {
    Box::pin(async move {
        let user_id = ctx.user_id();
        Ok(format!("You are assisting user {}.", user_id))
    })
})
```

### Callbacks

Intercept agent behavior:

```rust
.before_model_callback(|ctx, request| {
    Box::pin(async move {
        println!("About to call LLM with {} messages", request.contents.len());
        Ok(BeforeModelResult::Continue)
    })
})
```

---

## Builder Reference

| Method | Description |
|--------|-------------|
| `new(name)` | Creates builder with agent name |
| `model(Arc<dyn Llm>)` | Sets the LLM (required) |
| `description(text)` | Agent description |
| `instruction(text)` | System prompt |
| `tool(Arc<dyn Tool>)` | Adds a tool |
| `output_schema(json)` | JSON schema for structured output |
| `output_key(key)` | Saves response to state |
| `include_contents(mode)` | History visibility |
| `max_iterations(n)` | Maximum LLM round-trips (default: 100) |
| `build()` | Creates the agent |

### Iteration Control

The `max_iterations()` method limits how many LLM round-trips an agent can make before stopping. This is useful for:
- Preventing runaway tool-calling loops
- Controlling costs in production
- Setting reasonable bounds for complex tasks

```rust
let agent = LlmAgentBuilder::new("bounded_agent")
    .model(Arc::new(model))
    .instruction("You are a helpful assistant.")
    .tool(Arc::new(my_tool))
    .max_iterations(10)  // Stop after 10 LLM calls
    .build()?;
```

The default is 100 iterations, which is sufficient for most use cases. Lower values (5-20) are recommended for simple Q&A agents, while higher values may be needed for complex multi-step reasoning tasks.

---

## Complete Example

A production-ready agent with multiple tools (weather, calculator, search) and output saved to session state:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Weather tool
    let weather = FunctionTool::new(
        "get_weather",
        "Get weather for a city. Parameters: city (string)",
        |_ctx, args| async move {
            let city = args.get("city").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({
                "city": city,
                "temperature": "22°C",
                "humidity": "65%",
                "condition": "partly cloudy"
            }))
        },
    );

    // Calculator tool
    let calc = FunctionTool::new(
        "calculate",
        "Math operations. Parameters: expression (string like '2 + 2')",
        |_ctx, args| async move {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            Ok(json!({ "expression": expr, "result": "computed" }))
        },
    );

    // Build the full agent
    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful assistant with weather and calculation abilities")
        .instruction("You are a helpful assistant. \
                     Use the weather tool for weather questions. \
                     Use the calculator for math. \
                     Be concise and friendly.")
        .model(Arc::new(model))
        .tool(Arc::new(weather))
        .tool(Arc::new(calc))
        // .tool(Arc::new(GoogleSearchTool::new()))  // Currently unsupported with FunctionTool
        .output_key("last_response")
        .build()?;

    println!("✅ Agent '{}' ready!", agent.name());
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

Try these prompts:

```
You: What's 25 times 4?
Assistant: It's 100.

You: How's the weather in New York?
Assistant: The weather in New York is partly cloudy with a temperature of 22°C and 65% humidity.

You: Calculate 15% tip on $85
Assistant: A 15% tip on $85 is $12.75, making the total $97.75.
```

---

## Related

- [Workflow Agents](workflow-agents.md) - Sequential, Parallel, and Loop agents
- [Multi-Agent Systems](multi-agent.md) - Building agent hierarchies
- [Function Tools](../tools/function-tools.md) - Creating custom tools
- [Callbacks](../callbacks/callbacks.md) - Intercepting agent behavior

---

**Previous**: [Quickstart](../quickstart.md) | **Next**: [Workflow Agents →](workflow-agents.md)
