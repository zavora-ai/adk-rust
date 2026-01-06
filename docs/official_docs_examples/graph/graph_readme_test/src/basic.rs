//! Validates adk-graph README examples compile correctly

use adk_graph::{
    edge::{END, START, Router},
    node::{AgentNode, ExecutionConfig, NodeOutput},
    agent::GraphAgent,
    graph::StateGraph,
    state::State,
    checkpoint::MemoryCheckpointer,
};
use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use serde_json::json;
use std::sync::Arc;
use std::collections::HashMap;

// Validate: GraphAgent builder pattern
fn _graph_agent_builder() {
    let _agent = GraphAgent::builder("processor")
        .description("Process data")
        .channels(&["input", "output"])
        .node_fn("fetch", |_ctx| async move {
            Ok(NodeOutput::new().with_update("data", json!({"items": [1, 2, 3]})))
        })
        .edge(START, "fetch")
        .edge("fetch", END)
        .build();
}

// Validate: AgentNode with mappers
fn _agent_node_example() {
    let agent = LlmAgentBuilder::new("test").build().unwrap();
    
    let _node = AgentNode::new(Arc::new(agent))
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    if !text.is_empty() {
                        updates.insert("output".to_string(), json!(text));
                    }
                }
            }
            updates
        });
}

// Validate: StateGraph with conditional edges
fn _state_graph_example() {
    let _graph = StateGraph::with_channels(&["input", "sentiment", "response"])
        .add_node_fn("classifier", |_ctx| async move {
            Ok(NodeOutput::new().with_update("sentiment", json!("positive")))
        })
        .add_node_fn("handler", |_ctx| async move {
            Ok(NodeOutput::new())
        })
        .add_edge(START, "classifier")
        .add_conditional_edges(
            "classifier",
            Router::by_field("sentiment"),
            [("positive", "handler"), ("negative", "handler")],
        )
        .add_edge("handler", END)
        .compile();
}

// Validate: ExecutionConfig
fn _execution_config_example() {
    let _config = ExecutionConfig::new("thread-1")
        .with_recursion_limit(10)
        .with_metadata("key", json!("value"));
}

// Validate: NodeOutput methods
fn _node_output_example() {
    let _output = NodeOutput::new()
        .with_update("key", json!("value"));
    
    let _interrupt = NodeOutput::interrupt("Human approval required");
    let _interrupt_data = NodeOutput::interrupt_with_data("Paused", json!({"reason": "review"}));
}

// Validate: MemoryCheckpointer
fn _checkpointer_example() {
    let _checkpointer = MemoryCheckpointer::new();
}

// Validate: State operations
fn _state_example() {
    let mut state = State::new();
    state.insert("input".to_string(), json!("hello"));
    let _val = state.get("input");
}

fn main() {
    println!("✓ GraphAgent::builder() compiles");
    println!("✓ .description() compiles");
    println!("✓ .channels() compiles");
    println!("✓ .node_fn() compiles");
    println!("✓ .edge(START, ...) compiles");
    println!("✓ .edge(..., END) compiles");
    println!("✓ AgentNode::new() compiles");
    println!("✓ .with_input_mapper() compiles");
    println!("✓ .with_output_mapper() compiles");
    println!("✓ StateGraph::with_channels() compiles");
    println!("✓ .add_node_fn() compiles");
    println!("✓ .add_edge() compiles");
    println!("✓ .add_conditional_edges() compiles");
    println!("✓ Router::by_field() compiles");
    println!("✓ .compile() compiles");
    println!("✓ ExecutionConfig::new() compiles");
    println!("✓ .with_recursion_limit() compiles");
    println!("✓ NodeOutput::new() compiles");
    println!("✓ .with_update() compiles");
    println!("✓ NodeOutput::interrupt() compiles");
    println!("✓ NodeOutput::interrupt_with_data() compiles");
    println!("✓ MemoryCheckpointer::new() compiles");
    println!("✓ State::new() compiles");
    println!("\nadk-graph README validation passed!");
}
