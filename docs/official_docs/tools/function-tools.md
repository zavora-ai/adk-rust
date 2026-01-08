# Function Tools

Extend agent capabilities with custom Rust functions.

---

## What are Function Tools?

Function tools let you give agents abilities beyond conversation - calling APIs, performing calculations, accessing databases, or any custom logic. The LLM decides when to use a tool based on the user's request.

> **Key highlights**:
> - 🔧 **Wrap any async function** as a callable tool
> - 📝 **JSON parameters** - flexible input/output
> - 🎯 **Type-safe schemas** - optional JSON Schema validation
> - 🔗 **Context access** - session state, artifacts, memory

---

## Step 1: Basic Tool

Create a tool with `FunctionTool::new()` and **always add a schema** so the LLM knows what parameters to pass:

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize)]
struct WeatherParams {
    /// The city or location to get weather for
    location: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Weather tool with proper schema
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get current weather for a location",
        |_ctx, args| async move {
            let location = args.get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": "22°C",
                "conditions": "sunny"
            }))
        },
    )
    .with_parameters_schema::<WeatherParams>(); // Required for LLM to call correctly!

    let agent = LlmAgentBuilder::new("weather_agent")
        .instruction("You help users check the weather. Always use the get_weather tool.")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

> ⚠️ **Important**: Always use `.with_parameters_schema<T>()` - without it, the LLM won't know what parameters to pass and may not call the tool.

**How it works**:
1. User asks: "What's the weather in Tokyo?"
2. LLM decides to call `get_weather` with `{"location": "Tokyo"}`
3. Tool returns `{"location": "Tokyo", "temperature": "22°C", "conditions": "sunny"}`
4. LLM formats response: "The weather in Tokyo is sunny at 22°C."

---

## Step 2: Parameter Handling

Extract parameters from the JSON `args`:

```rust
let order_tool = FunctionTool::new(
    "process_order",
    "Process an order. Parameters: product_id (required), quantity (required), priority (optional)",
    |_ctx, args| async move {
        // Required parameters - return error if missing
        let product_id = args.get("product_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("product_id is required".into()))?;
        
        let quantity = args.get("quantity")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| adk_core::AdkError::Tool("quantity is required".into()))?;
        
        // Optional parameter with default
        let priority = args.get("priority")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");
        
        Ok(json!({
            "order_id": "ORD-12345",
            "product_id": product_id,
            "quantity": quantity,
            "priority": priority,
            "status": "confirmed"
        }))
    },
);
```

---

## Step 3: Typed Parameters with Schema

For complex tools, use typed structs with JSON Schema:

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Serialize, Deserialize)]
struct CalculatorParams {
    /// The arithmetic operation to perform
    operation: Operation,
    /// First operand
    a: f64,
    /// Second operand
    b: f64,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

let calculator = FunctionTool::new(
    "calculator",
    "Perform arithmetic operations",
    |_ctx, args| async move {
        let params: CalculatorParams = serde_json::from_value(args)?;
        let result = match params.operation {
            Operation::Add => params.a + params.b,
            Operation::Subtract => params.a - params.b,
            Operation::Multiply => params.a * params.b,
            Operation::Divide if params.b != 0.0 => params.a / params.b,
            Operation::Divide => return Err(adk_core::AdkError::Tool("Cannot divide by zero".into())),
        };
        Ok(json!({ "result": result }))
    },
)
.with_parameters_schema::<CalculatorParams>();
```

The schema is auto-generated from Rust types using `schemars`.

---

## Step 4: Multi-Tool Agent

Add multiple tools to one agent:

```rust
let agent = LlmAgentBuilder::new("assistant")
    .instruction("Help with calculations, conversions, and weather.")
    .model(Arc::new(model))
    .tool(Arc::new(calc_tool))
    .tool(Arc::new(convert_tool))
    .tool(Arc::new(weather_tool))
    .build()?;
```

The LLM automatically chooses the right tool based on the user's request.

---

## Error Handling

Return `AdkError::Tool` for tool-specific errors:

```rust
let divide_tool = FunctionTool::new(
    "divide",
    "Divide two numbers",
    |_ctx, args| async move {
        let a = args.get("a").and_then(|v| v.as_f64())
            .ok_or_else(|| adk_core::AdkError::Tool("Parameter 'a' is required".into()))?;
        let b = args.get("b").and_then(|v| v.as_f64())
            .ok_or_else(|| adk_core::AdkError::Tool("Parameter 'b' is required".into()))?;
        
        if b == 0.0 {
            return Err(adk_core::AdkError::Tool("Cannot divide by zero".into()));
        }
        
        Ok(json!({ "result": a / b }))
    },
);
```

Error messages are passed to the LLM, which can retry or ask for different input.

---

## Tool Context

Access session info via `ToolContext`:

```rust
#[derive(JsonSchema, Serialize, Deserialize)]
struct GreetParams {
    #[serde(default)]
    message: Option<String>,
}

let greet_tool = FunctionTool::new(
    "greet",
    "Greet the user with session info",
    |ctx, _args| async move {
        let user_id = ctx.user_id();
        let session_id = ctx.session_id();
        let agent_name = ctx.agent_name();
        Ok(json!({
            "greeting": format!("Hello, user {}!", user_id),
            "session": session_id,
            "served_by": agent_name
        }))
    },
)
.with_parameters_schema::<GreetParams>();
```

**Available context**:
- `ctx.user_id()` - Current user ID
- `ctx.session_id()` - Current session ID
- `ctx.agent_name()` - Name of the agent
- `ctx.artifacts()` - Access to artifact storage
- `ctx.search_memory(query)` - Search memory service

---

## Long-Running Tools

For operations that take significant time (data processing, external APIs), use the non-blocking pattern:

1. **Start tool** returns immediately with a task_id
2. **Background work** runs asynchronously  
3. **Status tool** lets users check progress

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(JsonSchema, Serialize, Deserialize)]
struct ReportParams {
    topic: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
struct StatusParams {
    task_id: String,
}

// Shared task store
let tasks: Arc<RwLock<HashMap<String, TaskState>>> = Arc::new(RwLock::new(HashMap::new()));
let tasks1 = tasks.clone();
let tasks2 = tasks.clone();

// Tool 1: Start (returns immediately)
let start_tool = FunctionTool::new(
    "generate_report",
    "Start generating a report. Returns task_id immediately.",
    move |_ctx, args| {
        let tasks = tasks1.clone();
        async move {
            let topic = args.get("topic").and_then(|v| v.as_str()).unwrap_or("general").to_string();
            let task_id = format!("task_{}", rand::random::<u32>());
            
            // Store initial state
            tasks.write().await.insert(task_id.clone(), TaskState {
                status: "processing".to_string(),
                progress: 0,
                result: None,
            });

            // Spawn background work (non-blocking!)
            let tasks_bg = tasks.clone();
            let tid = task_id.clone();
            tokio::spawn(async move {
                // Simulate work...
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                if let Some(t) = tasks_bg.write().await.get_mut(&tid) {
                    t.status = "completed".to_string();
                    t.result = Some("Report complete".to_string());
                }
            });

            // Return immediately with task_id
            Ok(json!({"task_id": task_id, "status": "processing"}))
        }
    },
)
.with_parameters_schema::<ReportParams>()
.with_long_running(true);  // Mark as long-running

// Tool 2: Check status
let status_tool = FunctionTool::new(
    "check_report_status",
    "Check report generation status",
    move |_ctx, args| {
        let tasks = tasks2.clone();
        async move {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(t) = tasks.read().await.get(task_id) {
                Ok(json!({"status": t.status, "result": t.result}))
            } else {
                Ok(json!({"error": "Task not found"}))
            }
        }
    },
)
.with_parameters_schema::<StatusParams>();
```

**Key points**:
- `.with_long_running(true)` tells the agent this tool returns a pending status
- The tool spawns work with `tokio::spawn()` and returns immediately
- Provide a status check tool so users can poll progress
```

This adds a note to prevent the LLM from calling the tool repeatedly.

---

## Run Examples

```bash
cd official_docs_examples/tools/function_tools_test

# Basic tool with closure
cargo run --bin basic

# Tool with typed JSON schema
cargo run --bin with_schema

# Multi-tool agent (3 tools)
cargo run --bin multi_tool

# Tool context (session info)
cargo run --bin context

# Long-running tool
cargo run --bin long_running
```

---

## Best Practices

1. **Clear descriptions** - Help the LLM understand when to use the tool
2. **Validate inputs** - Return helpful error messages for missing parameters
3. **Return structured JSON** - Use clear field names
4. **Keep tools focused** - Each tool should do one thing well
5. **Use schemas** - For complex tools, define parameter schemas

---

## Related

- [Built-in Tools](built-in-tools.md) - Pre-built tools (GoogleSearch, ExitLoop)
- [MCP Tools](mcp-tools.md) - Model Context Protocol integration
- [LlmAgent](../agents/llm-agent.md) - Adding tools to agents

---

**Previous**: [← mistral.rs](../models/mistralrs.md) | **Next**: [Built-in Tools →](built-in-tools.md)
