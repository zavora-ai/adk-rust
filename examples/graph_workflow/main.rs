//! Simple Graph Workflow Example
//!
//! This example demonstrates a basic graph workflow using function nodes
//! to process data through a pipeline. No LLM required.
//!
//! Graph: START -> extract -> analyze -> format -> END

use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Simple Graph Workflow Example ===\n");

    // Build a text analysis pipeline
    let graph = StateGraph::with_channels(&["text", "word_count", "char_count", "summary"])
        // Step 1: Extract basic metrics
        .add_node_fn("extract", |ctx| async move {
            let text = ctx.get("text").and_then(|v| v.as_str()).unwrap_or("");
            println!("[extract] Processing text: \"{}\"", &text[..text.len().min(50)]);

            let word_count = text.split_whitespace().count();
            let char_count = text.chars().count();

            Ok(NodeOutput::new()
                .with_update("word_count", json!(word_count))
                .with_update("char_count", json!(char_count)))
        })
        // Step 2: Analyze the text
        .add_node_fn("analyze", |ctx| async move {
            let text = ctx.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let word_count = ctx.get("word_count").and_then(|v| v.as_i64()).unwrap_or(0);

            println!("[analyze] Analyzing {} words...", word_count);

            // Simple analysis: calculate reading time and find first sentence
            let reading_time_mins = (word_count as f64 / 200.0).ceil() as i64;
            let first_sentence = text.split('.').next().unwrap_or(text).trim();

            Ok(NodeOutput::new()
                .with_update("reading_time", json!(reading_time_mins))
                .with_update("first_sentence", json!(first_sentence)))
        })
        // Step 3: Format the output
        .add_node_fn("format", |ctx| async move {
            let word_count = ctx.get("word_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let char_count = ctx.get("char_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let reading_time = ctx.get("reading_time").and_then(|v| v.as_i64()).unwrap_or(0);
            let first_sentence = ctx.get("first_sentence").and_then(|v| v.as_str()).unwrap_or("");

            println!("[format] Creating summary...");

            let summary = format!(
                "Text Analysis Summary:\n\
                 - Words: {}\n\
                 - Characters: {}\n\
                 - Estimated reading time: {} minute(s)\n\
                 - Preview: \"{}...\"",
                word_count, char_count, reading_time, first_sentence
            );

            Ok(NodeOutput::new().with_update("summary", json!(summary)))
        })
        // Define the flow: START -> extract -> analyze -> format -> END
        .add_edge(START, "extract")
        .add_edge("extract", "analyze")
        .add_edge("analyze", "format")
        .add_edge("format", END)
        .compile()?;

    // Test with sample text
    let sample_text = "The Rust programming language is blazingly fast and memory-efficient. \
        With no runtime or garbage collector, it can power performance-critical services. \
        Rust's rich type system and ownership model guarantee memory-safety and thread-safety. \
        This enables you to eliminate many classes of bugs at compile-time.";

    let mut input = State::new();
    input.insert("text".to_string(), json!(sample_text));

    let result = graph.invoke(input, ExecutionConfig::new("analysis-thread")).await?;

    println!("\n{}", result.get("summary").and_then(|v| v.as_str()).unwrap_or("No summary"));

    println!("\n=== Complete ===");
    Ok(())
}
