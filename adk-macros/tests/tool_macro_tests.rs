use adk_core::{AdkError, Tool};
use adk_macros::tool;
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
