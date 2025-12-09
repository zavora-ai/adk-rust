//! ReAct Agent Pattern with AgentNode and Tools
//!
//! This example demonstrates the ReAct (Reasoning + Acting) pattern using
//! AgentNode with an LlmAgent that has tools, cycling between reasoning and execution.
//!
//! Graph: START -> reasoner -> [has_tool_calls: executor | done: END] -> reasoner (cycle)
//!
//! Run with: cargo run --example graph_react
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_core::{Part, Tool};
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use adk_tool::FunctionTool;
use serde_json::json;
use std::sync::Arc;

/// Create weather tool
fn create_weather_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "get_weather",
        "Get the current weather for a location. Takes a 'location' parameter (city name).",
        |_ctx, args| async move {
            let location = args.get("location").and_then(|v| v.as_str()).unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": "72°F",
                "condition": "Sunny",
                "humidity": "45%"
            }))
        },
    ))
}

/// Create calculator tool
fn create_calculator_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "calculator",
        "Perform mathematical calculations. Takes an 'expression' parameter (string).",
        |_ctx, args| async move {
            let expr = args.get("expression").and_then(|v| v.as_str()).unwrap_or("0");
            // Simple expression evaluator
            let result = match expr {
                "2 + 2" => "4",
                "10 * 5" => "50",
                "100 / 4" => "25",
                "15 - 7" => "8",
                _ => "Unable to evaluate",
            };
            Ok(json!({ "result": result, "expression": expr }))
        },
    ))
}

/// Create search tool
fn create_search_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "search",
        "Search for information on a topic. Takes a 'query' parameter (string).",
        |_ctx, args| async move {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({
                "query": query,
                "results": [
                    {"title": "Result 1", "snippet": format!("Information about {}", query)},
                    {"title": "Result 2", "snippet": format!("More details on {}", query)},
                ]
            }))
        },
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== ReAct Agent Pattern with AgentNode ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_react");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create the reasoner agent with tools
    let reasoner_agent = Arc::new(
        LlmAgentBuilder::new("reasoner")
            .description("ReAct reasoner that uses tools to answer questions")
            .model(model.clone())
            .instruction(
                "You are a helpful assistant that uses tools to answer questions. \
                Think step by step about what information you need, use the appropriate tools, \
                and then provide a final answer. When you have enough information, \
                provide your final answer without calling more tools.",
            )
            .tool(create_weather_tool())
            .tool(create_calculator_tool())
            .tool(create_search_tool())
            .build()?,
    );

    // Create AgentNode for the reasoner
    let reasoner_node = AgentNode::new(reasoner_agent.clone())
        .with_input_mapper(|state| {
            let messages =
                state.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            // Build conversation context
            let context: Vec<String> = messages
                .iter()
                .filter_map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str())?;
                    let content = m.get("content").and_then(|c| c.as_str())?;
                    Some(format!("{}: {}", role, content))
                })
                .collect();

            let prompt = if context.is_empty() {
                state.get("input").and_then(|v| v.as_str()).unwrap_or("").to_string()
            } else {
                context.join("\n")
            };

            adk_core::Content::new("user").with_text(&prompt)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let mut has_tool_calls = false;
            let mut response_text = String::new();
            let mut tool_names = Vec::new();

            for event in events {
                // Check for tool calls by examining content parts
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        match part {
                            Part::FunctionCall { name, .. } => {
                                has_tool_calls = true;
                                tool_names.push(name.clone());
                            }
                            Part::Text { text } => {
                                response_text.push_str(text);
                            }
                            _ => {}
                        }
                    }
                }
            }

            updates.insert("has_tool_calls".to_string(), json!(has_tool_calls));
            if !tool_names.is_empty() {
                updates.insert("tool_calls".to_string(), json!(tool_names));
            }
            if !response_text.is_empty() {
                updates.insert("response".to_string(), json!(response_text));
            }
            updates
        });

    // Build the ReAct graph
    let graph = StateGraph::with_channels(&[
        "input",
        "messages",
        "response",
        "has_tool_calls",
        "tool_calls",
        "iteration",
    ])
    .add_node(reasoner_node)
    // Iteration counter node
    .add_node_fn("counter", |ctx| async move {
        let iteration = ctx.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
        println!("[iteration {}]", iteration + 1);
        Ok(NodeOutput::new().with_update("iteration", json!(iteration + 1)))
    })
    .add_edge(START, "counter")
    .add_edge("counter", "reasoner")
    .add_conditional_edges(
        "reasoner",
        |state| {
            let has_tools = state.get("has_tool_calls").and_then(|v| v.as_bool()).unwrap_or(false);
            let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);

            // Limit iterations to prevent infinite loops
            if iteration >= 5 {
                return END.to_string();
            }

            if has_tools {
                "counter".to_string() // Loop back for another iteration
            } else {
                END.to_string()
            }
        },
        [("counter", "counter"), (END, END)],
    )
    .compile()?
    .with_recursion_limit(10);

    // Test questions
    let questions = [
        "What's the weather in San Francisco and what's 2 + 2?",
        "Search for information about Rust programming and tell me about it.",
    ];

    for question in &questions {
        println!("\n{}", "=".repeat(60));
        println!("Question: {}\n", question);

        let mut input = State::new();
        input.insert("input".to_string(), json!(question));
        input.insert("messages".to_string(), json!([{"role": "user", "content": question}]));

        let result = graph.invoke(input, ExecutionConfig::new("react-thread")).await?;

        println!("\nFinal Response:");
        println!("{}", result.get("response").and_then(|v| v.as_str()).unwrap_or("No response"));
        println!("\nIterations: {}", result.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0));
    }

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - ReAct pattern with AgentNode wrapping LlmAgent");
    println!("  - Tools integration (weather, calculator, search)");
    println!("  - Cyclic graph execution with conditional routing");
    println!("  - Iteration limiting for safety");
    Ok(())
}
