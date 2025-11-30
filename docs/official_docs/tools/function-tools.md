# Function Tools

Function tools allow you to extend agent capabilities with custom Rust functions. They provide a way to give agents access to external APIs, databases, calculations, or any other functionality your application needs.

## Overview

A `FunctionTool` wraps an async Rust function and exposes it to the LLM as a callable tool. The LLM can decide when to call the tool based on its description and the conversation context.

Key features:
- **Async execution** - Tools run asynchronously and can perform I/O operations
- **JSON parameters** - Parameters are passed as `serde_json::Value` for flexibility
- **Type-safe schemas** - Optional JSON Schema for parameter validation
- **Context access** - Tools receive a `ToolContext` for accessing session state and artifacts

## Basic Usage

Create a function tool with `FunctionTool::new()`:

```rust
use adk_rust::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;

// Define the tool handler function
async fn get_weather(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let location = args.get("location")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    // In a real app, call a weather API here
    Ok(json!({
        "location": location,
        "temperature": 72,
        "conditions": "sunny"
    }))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create the function tool
    let weather_tool = FunctionTool::new(
        "get_weather",                           // Tool name
        "Get current weather for a location",    // Description for the LLM
        get_weather,                             // Handler function
    );

    // Add tool to agent
    let agent = LlmAgentBuilder::new("weather_agent")
        .model(Arc::new(model))
        .instruction("You help users check the weather. Use the get_weather tool.")
        .tool(Arc::new(weather_tool))
        .build()?;

    Ok(())
}
```

## Handler Function Signature

Tool handlers must follow this signature:

```rust
async fn handler(
    ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value>
```

Where:
- `ctx` - The tool context providing access to session information and artifacts
- `args` - JSON object containing the parameters passed by the LLM
- Returns `Result<Value>` - Success with JSON response or `AdkError`

### Using Closures

You can also use closures for simple tools:

```rust
let add_tool = FunctionTool::new(
    "add_numbers",
    "Add two numbers together",
    |_ctx: Arc<dyn ToolContext>, args: Value| async move {
        let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
        Ok(json!({ "result": a + b }))
    },
);
```

## Parameter Handling

Parameters are passed as a `serde_json::Value` object. Extract values using the `serde_json` API:

```rust
async fn process_order(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    // Required string parameter
    let product_id = args.get("product_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AdkError::Tool("product_id is required".into()))?;
    
    // Required integer parameter
    let quantity = args.get("quantity")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| AdkError::Tool("quantity is required".into()))?;
    
    // Optional parameter with default
    let priority = args.get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("normal");
    
    // Optional boolean
    let express = args.get("express")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    
    // Process the order...
    Ok(json!({
        "order_id": "ORD-12345",
        "product_id": product_id,
        "quantity": quantity,
        "priority": priority,
        "express": express,
        "status": "confirmed"
    }))
}
```

### Using Serde for Typed Parameters

For complex parameters, deserialize into a struct:

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct SearchParams {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    filters: Vec<String>,
}

fn default_limit() -> usize { 10 }

async fn search_documents(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let params: SearchParams = serde_json::from_value(args)
        .map_err(|e| AdkError::Tool(format!("Invalid parameters: {}", e)))?;
    
    // Use typed params
    println!("Searching for: {} (limit: {})", params.query, params.limit);
    
    Ok(json!({ "results": [], "total": 0 }))
}
```

## Return Values

Tools return `Result<Value>` where:
- `Ok(Value)` - Success response as JSON
- `Err(AdkError)` - Error that will be reported to the LLM

### Success Responses

Return any JSON-serializable data:

```rust
// Simple value
Ok(json!("Success"))

// Object
Ok(json!({
    "status": "completed",
    "data": { "id": 123 }
}))

// Array
Ok(json!(["item1", "item2", "item3"]))

// Using serde_json::to_value for structs
#[derive(Serialize)]
struct Response {
    id: String,
    created_at: String,
}

let response = Response {
    id: "abc123".into(),
    created_at: "2024-01-15".into(),
};
Ok(serde_json::to_value(response)?)
```

### Error Handling

Return `AdkError::Tool` for tool-specific errors:

```rust
async fn divide(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let a = args.get("a").and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("Parameter 'a' is required".into()))?;
    let b = args.get("b").and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("Parameter 'b' is required".into()))?;
    
    if b == 0.0 {
        return Err(AdkError::Tool("Cannot divide by zero".into()));
    }
    
    Ok(json!({ "result": a / b }))
}
```

The error message is passed back to the LLM, which can then decide how to handle it (retry, ask for different input, etc.).

## ToolContext Interface

The `ToolContext` provides access to execution context:

```rust
#[async_trait]
pub trait ToolContext: CallbackContext {
    /// Unique ID for this function call
    fn function_call_id(&self) -> &str;
    
    /// Actions that can modify session state
    fn actions(&self) -> &EventActions;
    
    /// Search the memory service
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}
```

### Inherited from CallbackContext

Through `CallbackContext`, you also have access to:

```rust
pub trait CallbackContext: ReadonlyContext {
    /// Access to artifact storage (if configured)
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
}
```

### Inherited from ReadonlyContext

```rust
pub trait ReadonlyContext {
    fn invocation_id(&self) -> &str;
    fn agent_name(&self) -> &str;
    fn user_id(&self) -> &str;
    fn app_name(&self) -> &str;
    fn session_id(&self) -> &str;
    fn branch(&self) -> &str;
    fn user_content(&self) -> &Content;
}
```

### Using Context in Tools

```rust
async fn personalized_greeting(
    ctx: Arc<dyn ToolContext>,
    _args: Value,
) -> Result<Value> {
    let user_id = ctx.user_id();
    let session_id = ctx.session_id();
    
    Ok(json!({
        "greeting": format!("Hello, user {}!", user_id),
        "session": session_id
    }))
}
```

## Parameter Schema

Define a JSON Schema for better LLM understanding of parameters:

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

// Create tool with typed schema
let calculator = FunctionTool::new(
    "calculator",
    "Perform arithmetic operations",
    calculator_handler,
)
.with_parameters_schema::<CalculatorParams>();
```

The schema is automatically generated from the Rust types using `schemars`.

## Response Schema

Similarly, define a response schema:

```rust
#[derive(JsonSchema, Serialize)]
struct CalculatorResult {
    /// The computed result
    result: f64,
    /// Human-readable expression
    expression: String,
}

let calculator = FunctionTool::new(
    "calculator",
    "Perform arithmetic operations",
    calculator_handler,
)
.with_parameters_schema::<CalculatorParams>()
.with_response_schema::<CalculatorResult>();
```

## Long-Running Tools

For tools that may take a long time to execute, mark them as long-running:

```rust
let slow_tool = FunctionTool::new(
    "generate_report",
    "Generate a comprehensive report (may take several minutes)",
    generate_report_handler,
)
.with_long_running(true);
```

> **Note**: Long-running tool support is currently limited. See the [roadmap](../../roadmap/long-running-tools.md) for planned enhancements.

## Complete Example

Here's a complete example with multiple tools:

```rust
use adk_rust::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;

// Calculator tool
async fn calculate(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let operation = args.get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("add");
    let a = args.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let b = args.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
    
    let result = match operation {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" if b != 0.0 => a / b,
        "divide" => return Err(AdkError::Tool("Cannot divide by zero".into())),
        _ => return Err(AdkError::Tool(format!("Unknown operation: {}", operation))),
    };
    
    Ok(json!({
        "result": result,
        "expression": format!("{} {} {} = {}", a, operation, b, result)
    }))
}

// Unit converter tool
async fn convert_units(
    _ctx: Arc<dyn ToolContext>,
    args: Value,
) -> Result<Value> {
    let value = args.get("value").and_then(|v| v.as_f64())
        .ok_or_else(|| AdkError::Tool("value is required".into()))?;
    let from = args.get("from").and_then(|v| v.as_str())
        .ok_or_else(|| AdkError::Tool("from unit is required".into()))?;
    let to = args.get("to").and_then(|v| v.as_str())
        .ok_or_else(|| AdkError::Tool("to unit is required".into()))?;
    
    let result = match (from, to) {
        ("celsius", "fahrenheit") => value * 9.0 / 5.0 + 32.0,
        ("fahrenheit", "celsius") => (value - 32.0) * 5.0 / 9.0,
        ("km", "miles") => value * 0.621371,
        ("miles", "km") => value / 0.621371,
        _ => return Err(AdkError::Tool(format!("Cannot convert {} to {}", from, to))),
    };
    
    Ok(json!({
        "original": { "value": value, "unit": from },
        "converted": { "value": result, "unit": to }
    }))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    let calc_tool = FunctionTool::new(
        "calculator",
        "Perform arithmetic: add, subtract, multiply, divide",
        calculate,
    );

    let convert_tool = FunctionTool::new(
        "convert_units",
        "Convert between units (temperature, distance)",
        convert_units,
    );

    let agent = LlmAgentBuilder::new("math_helper")
        .description("A helpful math and conversion assistant")
        .instruction("Help users with calculations and unit conversions. \
                     Use the calculator for arithmetic and convert_units for conversions.")
        .model(Arc::new(model))
        .tool(Arc::new(calc_tool))
        .tool(Arc::new(convert_tool))
        .build()?;

    println!("Created agent: {}", agent.name());
    Ok(())
}
```

## Best Practices

1. **Clear descriptions** - Write tool descriptions that help the LLM understand when to use the tool
2. **Validate inputs** - Always validate required parameters and return helpful error messages
3. **Return structured data** - Use JSON objects with clear field names
4. **Handle errors gracefully** - Return `AdkError::Tool` with descriptive messages
5. **Keep tools focused** - Each tool should do one thing well
6. **Use schemas** - Define parameter schemas for complex tools to improve LLM accuracy

## API Reference

See the rustdoc for `FunctionTool` for complete API documentation.

## Related

- [Built-in Tools](built-in-tools.md) - Pre-built tools like GoogleSearchTool
- [MCP Tools](mcp-tools.md) - Using MCP servers as tool providers
- [LlmAgent](../agents/llm-agent.md) - Adding tools to agents
- [Callbacks](../callbacks/callbacks.md) - Intercepting tool execution
