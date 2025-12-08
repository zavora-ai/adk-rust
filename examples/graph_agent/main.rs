//! GraphAgent Integration Example
//!
//! This example demonstrates using GraphAgent which implements the ADK Agent trait.
//! This allows graph workflows to be used anywhere an Agent is expected.

use adk_core::Agent;
use adk_graph::{
    agent::GraphAgent,
    edge::{END, START},
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== GraphAgent Integration Example ===\n");

    // Build a GraphAgent using the builder pattern
    let agent = GraphAgent::builder("calculator")
        .description("A simple calculation pipeline")
        .channels(&["a", "b", "sum", "product", "result"])
        // Add two numbers
        .node_fn("add", |ctx| async move {
            let a = ctx.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let b = ctx.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
            println!("[add] {} + {} = {}", a, b, a + b);
            Ok(NodeOutput::new().with_update("sum", json!(a + b)))
        })
        // Multiply the same numbers
        .node_fn("multiply", |ctx| async move {
            let a = ctx.get("a").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let b = ctx.get("b").and_then(|v| v.as_f64()).unwrap_or(0.0);
            println!("[multiply] {} * {} = {}", a, b, a * b);
            Ok(NodeOutput::new().with_update("product", json!(a * b)))
        })
        // Combine results
        .node_fn("combine", |ctx| async move {
            let sum = ctx.get("sum").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let product = ctx.get("product").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let result = format!("Sum: {}, Product: {}, Total: {}", sum, product, sum + product);
            println!("[combine] {}", result);
            Ok(NodeOutput::new().with_update("result", json!(result)))
        })
        // Define edges
        .edge(START, "add")
        .edge(START, "multiply") // Parallel entry
        .edge("add", "combine")
        .edge("multiply", "combine")
        .edge("combine", END)
        .build()?;

    // Use the Agent trait methods
    println!("Agent name: {}", agent.name());
    println!("Agent description: {}", agent.description());
    println!();

    // Test the calculation
    let mut input = State::new();
    input.insert("a".to_string(), json!(5.0));
    input.insert("b".to_string(), json!(3.0));

    println!("Input: a=5, b=3\n");

    let result = agent.invoke(input, ExecutionConfig::new("calc-thread")).await?;

    println!("\nFinal result: {}", result.get("result").and_then(|v| v.as_str()).unwrap_or("none"));

    println!("\n=== Complete ===");
    Ok(())
}
