use adk_core::{AdkError, Tool};
// Alias: the #[tool] macro generates code referencing adk_tool::{Tool, AdkError, ...}
// For testing without the circular dep, adk_core provides the same types.
use adk_core as adk_tool;
use adk_rust_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

// === Test 1: Basic tool with typed args ===

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to look up
    city: String,
}

/// Get the current weather for a city.
#[tool]
async fn get_weather(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "temp": 72, "city": args.city }))
}

#[test]
fn tool_has_correct_name() {
    let tool = GetWeather;
    assert_eq!(tool.name(), "get_weather");
}

#[test]
fn tool_has_doc_description() {
    let tool = GetWeather;
    assert_eq!(tool.description(), "Get the current weather for a city.");
}

#[test]
fn tool_has_schema() {
    let tool = GetWeather;
    let schema = tool.parameters_schema();
    assert!(schema.is_some());
    let schema = schema.unwrap();
    assert!(schema.get("properties").is_some());
}

// === Test 2: Tool with context parameter ===

/// Calculate a mathematical expression.
#[tool]
async fn calculator(
    _ctx: Arc<dyn adk_core::ToolContext>,
    args: CalcArgs,
) -> Result<Value, AdkError> {
    Ok(json!({ "result": args.a + args.b }))
}

#[derive(Deserialize, JsonSchema)]
struct CalcArgs {
    a: f64,
    b: f64,
}

#[test]
fn tool_with_context_has_correct_name() {
    let tool = Calculator;
    assert_eq!(tool.name(), "calculator");
}

#[test]
fn tool_with_context_has_schema() {
    let tool = Calculator;
    let schema = tool.parameters_schema().unwrap();
    let props = schema.get("properties").unwrap();
    assert!(props.get("a").is_some());
    assert!(props.get("b").is_some());
}

// === Test 3: Tool without doc comment gets auto-description ===

#[tool]
async fn my_cool_tool(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_without_doc_gets_auto_description() {
    let tool = MyCoolTool;
    assert_eq!(tool.description(), "my cool tool");
}

// === Test 4: Tool with read_only attribute ===

/// A read-only lookup tool.
#[tool(read_only)]
async fn read_only_lookup(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_read_only_attribute() {
    let tool = ReadOnlyLookup;
    assert!(tool.is_read_only(), "read_only attribute should set is_read_only() to true");
    assert!(!tool.is_concurrency_safe(), "concurrency_safe should remain false");
    assert!(!tool.is_long_running(), "long_running should remain false");
}

// === Test 5: Tool with multiple attributes ===

/// A concurrent read-only tool.
#[tool(read_only, concurrency_safe)]
async fn concurrent_lookup(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_multiple_attributes() {
    let tool = ConcurrentLookup;
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    assert!(!tool.is_long_running());
}

// === Test 6: Tool with long_running attribute ===

/// A long-running background task.
#[tool(long_running)]
async fn background_task(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_long_running_attribute() {
    let tool = BackgroundTask;
    assert!(!tool.is_read_only());
    assert!(!tool.is_concurrency_safe());
    assert!(tool.is_long_running());
}

// === Test 7: Tool with all attributes ===

/// A tool with every attribute set.
#[tool(read_only, concurrency_safe, long_running)]
async fn fully_attributed(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({ "city": args.city }))
}

#[test]
fn tool_all_attributes() {
    let tool = FullyAttributed;
    assert!(tool.is_read_only());
    assert!(tool.is_concurrency_safe());
    assert!(tool.is_long_running());
}

// === Test 8: Plain #[tool] still defaults to false ===

#[test]
fn tool_no_attributes_defaults_false() {
    let tool = GetWeather;
    assert!(!tool.is_read_only());
    assert!(!tool.is_concurrency_safe());
    assert!(!tool.is_long_running());
}
