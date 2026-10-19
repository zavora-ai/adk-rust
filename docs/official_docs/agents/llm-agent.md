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
adk-rust = "3.0.0"
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
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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

`Launcher` opens a console session: it streams the agent's answer after each `You >` prompt,
and `exit` or `quit` ends it.

```
  ADK-Rust — my_agent
  Type a message to chat. Type 'exit' to quit.

You > Hello! What can you help me with?
I can assist you with answering questions, writing, brainstorming, and solving problems.
Just let me know what you need help with today!

You > exit

Goodbye.
```

---

## Shaping Agent Behavior with Instructions

The `instruction()` method defines your agent's personality and behavior. It is the **system prompt**: `LlmAgent` sends it as a `system`-role content at the start of every request, and each provider places it in its native system prompt:

```rust
let model = Arc::new(model);

// A formal business assistant
let formal_agent = LlmAgentBuilder::new("formal_assistant")
    .instruction("You are a professional business consultant. \
                 Use formal language. Be concise and data-driven.")
    .model(model.clone())
    .build()?;

// A friendly coding tutor
let tutor_agent = LlmAgentBuilder::new("code_tutor")
    .instruction("You are a friendly coding tutor for beginners. \
                 Explain concepts simply. Use examples. \
                 Encourage questions. Never make the user feel bad for not knowing.")
    .model(model.clone())
    .build()?;

// A creative storyteller
let story_agent = LlmAgentBuilder::new("storyteller")
    .instruction("You are a creative storyteller. \
                 Craft engaging narratives with vivid descriptions. \
                 Use plot twists and memorable characters.")
    .model(model.clone())
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
use adk_rust::{EventTextDeltas, SessionId, UserId};
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

    // 1. Agent with templated instruction
    let agent = LlmAgentBuilder::new("personalized")
        .instruction("You are helping {user_name}. Their role is {user_role}. \
                     Tailor your responses to their expertise level.")
        .model(Arc::new(model))
        .build()?;

    // 2. Create session service and runner
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("templating_demo")
        .agent(Arc::new(agent))
        .session_service(session_service.clone())
        .build()?;

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
        UserId::new("user123")?,
        SessionId::new(session.id())?,
        Content::new("user").with_text("Explain async/await in Rust"),
    ).await?;

    // Print the response. Streamed deltas end with a snapshot of the whole answer;
    // EventTextDeltas turns that snapshot into the text not yet printed.
    let mut text_deltas = EventTextDeltas::default();
    while let Some(event) = response_stream.next().await {
        let event = event?;
        let event = text_deltas.push(&event);
        if let Some(content) = event.content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    print!("{text}");
                }
            }
        }
    }
    println!();

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

A required `{var}` that is missing from state fails the turn with `State variable 'var' not found`;
use `{var?}` when the value may be absent.

Text that is not a placeholder stays as written, so an instruction can quote code:

| Text | Sent to the model |
|------|-------------------|
| `${HOME}`, `` `${name}` `` | Unchanged — a brace after `$` opens no placeholder |
| `\{name}` | Unchanged — a brace after `\` opens no placeholder |
| `{{name}}`, `{{ name }}` | Unchanged — doubled braces open no placeholder |
| `{"key": 1}`, `{1}` | Unchanged — the content is not a variable name |

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
A `FunctionTool` built with `new` alone declares no parameters, so the examples on this page
name them in the description. To declare them, and have each call's arguments checked against
them, add `.with_parameters_schema::<Args>()` with a `schemars::JsonSchema` type; see
[Function Tools](../tools/function-tools.md).

Provider-native tools, such as `GoogleSearchTool`, can be mixed with `FunctionTool` instances in the same agent. ADK forwards the native tool declarations to the provider and executes the function tools locally. See [Built-in Tools](../tools/built-in-tools.md).

### Build a Multi-Tool Agent

Create a new project:

```bash
cargo new tool_agent
cd tool_agent
```

Add dependencies to `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["tools"] }
tokio = { version = "1.40", features = ["full"] }
dotenvy = "0.15"
serde_json = "1.0"
```

Create `.env`:

```bash
echo 'GOOGLE_API_KEY=your-api-key' > .env
```

Replace `src/main.rs` with an agent that has two tools:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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

    // Build agent with weather and calculator tools
    let agent = LlmAgentBuilder::new("multi_tool_agent")
        .instruction("You are a helpful assistant. Use tools when needed: \
                     - get_weather for weather questions \
                     - calculate for math")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .tool(Arc::new(calculator))
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

The launcher prints each tool call as `[tool] name(args)` before the answer:

```
You > What's 15% of 250? Use the calculator.

[tool] calculate({"a":0.15,"b":250,"operation":"multiply"})15% of 250 is **37.5**.

You > What's the weather in Tokyo?

[tool] get_weather({"city":"Tokyo"})The current weather in Tokyo is sunny with a temperature of 22°C.
```

---

## Structured Output with JSON Schema

For applications that need structured data, use `output_schema()`. This program extracts
entities from one sentence and deserializes the validated answer:

```rust
use adk_rust::agent::extract_typed;
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::{SessionId, UserId};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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
        .output_key("entities") // the validated JSON is also saved to state["entities"]
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("extractor")
        .agent(Arc::new(extractor))
        .session_service(sessions.clone())
        .build()?;
    let session = sessions
        .create(CreateRequest {
            app_name: "extractor".to_string(),
            user_id: "user".to_string(),
            session_id: None,
            state: Default::default(),
        })
        .await?;

    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session.id())?,
            Content::new("user").with_text("John met Sarah in Paris on December 25th"),
        )
        .await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }

    let entities: serde_json::Value = extract_typed(&events)?;
    println!("{entities:#}");
    Ok(())
}
```

### How Providers Enforce the Schema

`output_schema` reaches the provider as `GenerateContentConfig::response_schema`.
What the provider does with it differs, and the agent validates the result either
way:

| Provider | Native enforcement |
|----------|--------------------|
| Gemini | Full schema, sent as the response schema |
| OpenAI and OpenAI-compatible | Full schema, sent as a strict `json_schema` response format |
| OpenRouter | Full schema |
| Ollama | Full schema, sent as the request's `format` |
| DeepSeek | JSON **syntax** only — DeepSeek's JSON Output mode has no `json_schema` variant, so the schema is enforced by the agent's validation |
| Anthropic, Groq, Bedrock | None — the agent's instruction and validation enforce it |

Whatever the provider does, the agent adds the schema to the request as a `system`
instruction and validates the reply. A reply that fails validation is answered with a
correction prompt, up to `output_max_retries` times (default 3), so a non-conforming
answer costs a retry rather than returning bad data. Use `output_type::<T>()` to derive
the schema from a type that implements `schemars::JsonSchema`, and
`adk_rust::agent::extract_typed::<T>(&events)` to deserialize the validated answer.

A reply wrapped in one Markdown code fence (```` ```json ... ``` ````), which models
sometimes produce when tools are configured beside the schema, is unwrapped before
validation. `output_key` then stores the JSON without the fence, and `extract_typed`
unwraps it the same way.

### Model Turns Without an Answer

A model turn that ends with no tool call and no answer — no parts, only thinking, or only
whitespace — fails the run with a `Model` error coded `model.empty_response` instead of
finishing silently; its `details.metadata.finish_reason` names the provider's finish reason.
`output_key` is not written. The run is not retried automatically: retry the run, or check the
model's output limit and safety settings. A response a `before_model_callback` returned in place
of the model is not checked.

### Replacing a Streamed Response

An `after_model_callback` that returns a new `LlmResponse` replaces the chunk's content and
metadata, but the chunk keeps its place in the stream: `partial` and `turn_complete` stay set
when the original had them, and `finish_reason` and `usage_metadata` carry over unless the
replacement sets them. A callback that returns `LlmResponse { content, ..Default::default() }`
for each streamed delta therefore still produces one persisted event per response, with its
usage. A stream that ends on a partial chunk gets one closing event with the whole response,
so the session keeps it.

> **Note:** DeepSeek requires the word "json" to appear in the prompt whenever JSON
> Output is on, otherwise the API can return empty content. The adapter adds that
> mention itself when your prompt does not already contain it.

### JSON Output Example

The program above prints:

```json
{
  "dates": [
    "December 25th"
  ],
  "locations": [
    "Paris"
  ],
  "people": [
    "John",
    "Sarah"
  ]
}
```

---

## Advanced Features

### Include Contents

Control conversation history visibility:

```rust
use adk_rust::IncludeContents;

// Full history (default)
.include_contents(IncludeContents::Default)

// Stateless - sees only its instructions plus the current user turn
.include_contents(IncludeContents::None)
```

### Output Key

Save agent responses to session state:

```rust
.output_key("summary")  // Response saved to state["summary"]
```

`state["summary"]` holds the text of the agent's final answer: the last model turn, the one
without tool calls. Text the model writes beside a tool call in an earlier turn is not
included, and each run replaces the value. When a `before_agent` callback answers for the
agent, its content is the answer stored. Structured output (`output_schema`) is stored as
the JSON it validated as.

### Dynamic Instructions

Compute instructions at runtime. A provider replaces `instruction()`, and its result is sent
as written, without `{var}` substitution:

```rust
.instruction_provider(Box::new(|ctx| {
    Box::pin(async move {
        let user_id = ctx.user_id();
        Ok(format!("You are assisting user {user_id}."))
    })
}))
```

### Callbacks

Intercept agent behavior. See [Callbacks](../callbacks/callbacks.md) for every callback type:

```rust
.before_model_callback(Box::new(|_ctx, request| {
    Box::pin(async move {
        println!("About to call the model with {} contents", request.contents.len());
        Ok(BeforeModelResult::Continue(request))
    })
}))
```

---

## Builder Reference

| Method | Description |
|--------|-------------|
| `new(name)` | Creates builder with agent name |
| `model(Arc<dyn Llm>)` | Sets the LLM (required) |
| `description(text)` | Agent description |
| `instruction(text)` | System prompt, with `{var}` templating |
| `instruction_provider(provider)` | Computes the system prompt per request |
| `global_instruction(text)` | Instruction sent before `instruction`, for this agent only |
| `sub_agent(Arc<dyn Agent>)` | Adds a transfer target (see [Multi-Agent Systems](multi-agent.md)) |
| `tool(Arc<dyn Tool>)` | Adds a static tool |
| `toolset(Arc<dyn Toolset>)` | Adds a dynamic toolset resolved per invocation |
| `strict_toolsets(bool)` | Fails the turn when a toolset cannot resolve its tools (default: skip it) |
| `output_schema(json)` | JSON schema for structured output |
| `output_type::<T>()` | Derives `output_schema` from a `schemars::JsonSchema` type |
| `output_max_retries(n)` | Correction retries after a reply fails the schema (default: 3) |
| `output_key(key)` | Saves response to state |
| `include_contents(mode)` | History visibility |
| `max_iterations(n)` | Maximum LLM round-trips (default: 100) |
| `tool_timeout(Duration)` | Per-call tool timeout (default: 5 minutes) |
| `require_tool_confirmation(name)` | Holds calls to the tool for approval (see [Tool Authorization](../security/tool-authorization.md)) |
| `tool_execution_strategy(strategy)` | Tool dispatch mode: `Sequential`, `Parallel`, or `Auto` |
| `parallelize_agent_delegations(bool)` | Overlap consecutive agent-delegation calls without parallelizing ordinary tools |
| `default_retry_budget(RetryBudget)` | Retry failed read-only and idempotent tools up to N times with exponential backoff |
| `tool_retry_budget(name, RetryBudget)` | Per-tool retry override |
| `circuit_breaker_threshold(u32)` | Disable tool after N consecutive failures |
| `on_tool_error(callback)` | Register fallback handler for tool failures |
| `expose_tool_error_messages(bool)` | Send tool and tool-callback error messages to the model (default: component, category, and code only) |
| `after_tool_callback_full(callback)` | V2 rich after-tool callback with tool, args, and response |
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

## Dynamic Toolsets

For tools that depend on invocation context (e.g., per-user browser sessions), use `.toolset()` instead of `.tool()`. Toolsets are resolved at the start of each `run()` call, and again after each round of tool calls:

```rust
use adk_browser::{BrowserSessionPool, BrowserToolset, BrowserConfig};
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::new(), 10));
let toolset = Arc::new(BrowserToolset::with_pool(pool));

let agent = LlmAgentBuilder::new("web_agent")
    .model(model)
    .instruction("You are a web automation assistant.")
    .toolset(toolset)  // Resolved per-user at runtime
    .build()?;
```

You can mix static `.tool()` and dynamic `.toolset()` on the same agent. Two static tools with one name fail `build()` with code `agent.duplicate_tool_name` (`adk_core::DUPLICATE_TOOL_NAME_CODE`); a name shared between a static tool and a toolset, or between two toolsets, fails the turn when the toolsets resolve.

A toolset whose `tools()` call fails, such as an MCP server that is down, is skipped for that resolution: the agent logs a `warn` naming the toolset and the error, and the model runs with the tools that did resolve. The toolset is tried again at the next resolution. Enable strict mode when the agent must not run without every toolset:

```rust
let agent = LlmAgentBuilder::new("operator")
    .model(model)
    .toolset(Arc::new(mcp_toolset))
    .strict_toolsets(true) // a failing toolset fails the turn
    .build()?;
```

Strict mode covers toolsets added with `.toolset()` and `RunConfig::runtime_toolsets`. Duplicate tool names fail the turn in both modes.

`RealtimeAgentBuilder` also supports `.toolset()` with the same semantics, so realtime voice agents get dynamic tool resolution too.

### Closing Toolsets

`Runner::close` releases what an agent tree holds: it calls `Agent::close` on the root agent,
then closes the runner's plugins. `LlmAgent::close` closes each of its toolsets through
`Toolset::close`, then its enhanced plugins, then its sub-agents. `McpToolset` stops its
server process or ends its HTTP session there, and `FilteredToolset`, `MergedToolset` and
`PrefixedToolset` forward the call; the default `Toolset::close` does nothing, so implement it
for a toolset of your own that holds a connection or a subprocess. Call `runner.close().await`
once the application stops running the agent.

### Toolset Composition

Use `FilteredToolset`, `MergedToolset`, and `PrefixedToolset` from `adk-tool` to compose complex toolset configurations:

```rust
use adk_tool::{BasicToolset, FilteredToolset, MergedToolset, PrefixedToolset, string_predicate};

// Prefix weather tools to avoid name collisions
let weather = Arc::new(PrefixedToolset::new(weather_toolset, "wx"));

// Filter utility tools to only expose search and calculate
let utils = Arc::new(FilteredToolset::new(
    utility_toolset,
    string_predicate(vec!["search".into(), "calculate".into()]),
));

// Merge into a single toolset
let composed = MergedToolset::new("all", vec![weather, utils]);

let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .toolset(Arc::new(composed))
    .build()?;
```

All composition utilities work with any `Toolset` implementation including `McpToolset` and `BrowserToolset`.

## Parallel Tool Execution

When an LLM returns multiple tool calls in a single response, you can control how they're dispatched:

```rust
use adk_core::ToolExecutionStrategy;

let agent = LlmAgentBuilder::new("fast_agent")
    .model(Arc::new(model))
    .instruction("You are a research assistant. Use multiple tools in parallel.")
    // Auto requires both safety signals for concurrent inclusion
    .tool_execution_strategy(ToolExecutionStrategy::Auto)
    .tool(Arc::new(
        search_tool
            .with_read_only(true)
            .with_concurrency_safe(true),
    ))
    .tool(Arc::new(
        lookup_tool
            .with_read_only(true)
            .with_concurrency_safe(true),
    ))
    .tool(Arc::new(save_tool)) // runs after the concurrent safe subset
    .build()?;
```

Three strategies are available:

- `Sequential` (default) — tools execute one at a time in LLM order
- `Parallel` — all tools execute concurrently; this explicit override bypasses safety metadata, so the caller owns safety
- `Auto` — calls whose tools are both read-only and concurrency-safe run concurrently first; all remaining calls then run sequentially

Results are always returned in the original LLM order regardless of strategy. Failed tools produce a JSON error response without aborting the batch.

The strategy is set per-agent via `LlmAgentBuilder::tool_execution_strategy()`. If not set, the default is `Sequential`.
Use `LlmAgentBuilder::parallelize_agent_delegations(true)` when only consecutive agent-delegation calls should overlap. Ordinary tools remain sequential and keep their original phase ordering.

## Tool Resilience

Configure retry budgets and circuit breakers for production agents:

```rust
use adk_core::RetryBudget;
use std::time::Duration;

let agent = LlmAgentBuilder::new("resilient_agent")
    .model(model)
    .tool(Arc::new(my_tool))
    // Retry read-only and idempotent tools up to 2 times, starting at 500ms
    .default_retry_budget(RetryBudget::new(2, Duration::from_millis(500)))
    // Override for a specific tool
    .tool_retry_budget("flaky_api", RetryBudget::new(4, Duration::from_secs(1)))
    // Disable a tool after 3 consecutive failures in one invocation
    .circuit_breaker_threshold(3)
    // Provide a fallback when a tool fails
    .on_tool_error(Box::new(|_ctx, tool, _args, error| {
        Box::pin(async move {
            tracing::warn!(tool = tool.name(), %error, "tool failed");
            Ok(None) // None = the default error response; Some(value) = use as fallback
        })
    }))
    .build()?;
```

A retry budget applies only to tools whose `effect()` is `ReadOnly` or `Idempotent`, and only after a retryable error (rate limited, unavailable, timed out). A `NonIdempotent` tool — the default for any tool not marked read-only — runs once. The delay doubles on each retry up to `RetryBudget::max_delay`, with jitter. A `NonIdempotent` call that times out is answered with `{"status": "outcome_unknown"}` rather than an error, and an `AgentTool` delegation is bounded by its own `timeout` rather than `tool_timeout`. See [Tool Effects and the Action Ledger](../tools/tool-effects.md).

### What the model is told

Every function call gets exactly one function response, which is sent to the model provider and
stored in the session. An error message can quote credentials, internal hosts, or upstream bodies,
so by default the model sees an error's component, category, and code
(`AdkError::redacted()`), not its message:

| Outcome | Function response |
|---------|-------------------|
| Tool returns `Err(AdkError)` | `{"error": "tool 'lookup_order' failed: tool.unavailable (tool.orders_db.unavailable)"}` |
| Error with code `adk_core::TOOL_INVALID_ARGUMENTS_CODE` (the `#[tool]` macro's argument errors) | `{"error": "tool.invalid_input: invalid arguments for 'lookup_order': missing field `id`"}` — the message tells the model what to fix |
| Timeout | `{"error": "Tool 'slow_report' timed out after 50ms"}`; a `NonIdempotent` call gets `{"status": "outcome_unknown", ...}` |
| Before-tool callback or plugin returns `Err` | `{"error": "tool 'lookup_order' was not run: a before-tool callback failed with agent.internal (agent.legacy)"}` |
| After-tool hook, callback, or plugin returns `Err` | `{"status": "result_withheld", "error": "tool 'charge_card' ran, but its after-tool pipeline failed with ..., so its result is withheld; do not call it again for this request."}` |
| The provider could not decode the model's arguments into a JSON object | `{"error": "tool 'get_weather' was not run: arguments are not valid JSON: ... Call it again with arguments that are one JSON object matching its parameters."}` — the tool does not run and the turn continues |

- **Full messages**: `expose_tool_error_messages(true)` sends `component.category: message`
  instead. Enable it only when every tool's error messages are safe to send to the provider.
- **Where the message goes**: `on_tool_error` receives the full message, and
  `ToolOutcome::error` carries the structured `AdkError`. Tracing records the message when
  `RunConfig::record_payloads` is set.
- **After-tool failures**: the first failing stage ends the after-tool pipeline, since a later stage
  may be handed the unscreened result. The tool's side effect stands and its action-ledger record
  says it completed; the model is told not to repeat the call, because a new call has a new
  idempotency key.
- **Errors the model should read**: a tool returns `Ok(json!({"error": "..."}))` for a failure
  whose text is meant for the model, or an `on_tool_error` callback supplies that text.

After-tool callbacks can inspect structured `ToolOutcome` metadata via `CallbackContext::tool_outcome()`:

```rust
.after_tool_callback(Box::new(|ctx| {
    Box::pin(async move {
        if let Some(outcome) = ctx.tool_outcome() {
            println!(
                "Tool '{}' {} in {:?} (attempt {})",
                outcome.tool_name,
                if outcome.success { "succeeded" } else { "failed" },
                outcome.duration,
                outcome.attempt,
            );
        }
        Ok(None)
    })
}))
```

---

## Complete Example

An agent with weather and calculator tools whose final answer is saved to session state. It needs the `tools` feature, as in [Build a Multi-Tool Agent](#build-a-multi-tool-agent):

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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
        .output_key("last_response")
        .build()?;

    println!("Agent '{}' ready", agent.name());
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

Try a prompt such as "How's the weather in New York?".

---

## Related

- [Workflow Agents](workflow-agents.md) - Sequential, Parallel, and Loop agents
- [Multi-Agent Systems](multi-agent.md) - Building agent hierarchies
- [Function Tools](../tools/function-tools.md) - Creating custom tools
- [Callbacks](../callbacks/callbacks.md) - Intercepting agent behavior

---

**Previous**: [Quickstart](../quickstart.md) | **Next**: [Workflow Agents →](workflow-agents.md)
