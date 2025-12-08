//! Conditional Routing Graph Example
//!
//! This example demonstrates conditional branching in a graph workflow.
//! Based on input sentiment, it routes to different processing nodes.
//!
//! Graph:
//!   START -> classify -> [positive | negative | neutral] -> respond -> END

use adk_graph::{
    edge::{Router, END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Conditional Routing Graph Example ===\n");

    // Build a sentiment-based routing workflow
    let graph = StateGraph::with_channels(&["message", "sentiment", "response"])
        // Step 1: Classify the sentiment
        .add_node_fn("classify", |ctx| async move {
            let message = ctx.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let message_lower = message.to_lowercase();

            println!("[classify] Analyzing: \"{}\"", message);

            // Simple keyword-based sentiment classification
            let sentiment = if message_lower.contains("great")
                || message_lower.contains("excellent")
                || message_lower.contains("amazing")
                || message_lower.contains("love")
                || message_lower.contains("happy")
            {
                "positive"
            } else if message_lower.contains("bad")
                || message_lower.contains("terrible")
                || message_lower.contains("hate")
                || message_lower.contains("awful")
                || message_lower.contains("angry")
            {
                "negative"
            } else {
                "neutral"
            };

            println!("[classify] Sentiment: {}", sentiment);
            Ok(NodeOutput::new().with_update("sentiment", json!(sentiment)))
        })
        // Positive response handler
        .add_node_fn("positive", |ctx| async move {
            let message = ctx.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("[positive] Generating positive response...");
            Ok(NodeOutput::new().with_update(
                "response",
                json!(format!(
                    "Thank you for the positive feedback! We're thrilled you enjoyed: \"{}\"",
                    message
                )),
            ))
        })
        // Negative response handler
        .add_node_fn("negative", |ctx| async move {
            let message = ctx.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("[negative] Generating supportive response...");
            Ok(NodeOutput::new().with_update(
                "response",
                json!(format!(
                    "We're sorry to hear about your experience. Your feedback \"{}\" helps us improve.",
                    message
                )),
            ))
        })
        // Neutral response handler
        .add_node_fn("neutral", |ctx| async move {
            let message = ctx.get("message").and_then(|v| v.as_str()).unwrap_or("");
            println!("[neutral] Generating neutral response...");
            Ok(NodeOutput::new().with_update(
                "response",
                json!(format!(
                    "Thank you for your message: \"{}\". How can we assist you further?",
                    message
                )),
            ))
        })
        // Final response node
        .add_node_fn("respond", |ctx| async move {
            let response = ctx.get("response").and_then(|v| v.as_str()).unwrap_or("");
            let sentiment = ctx.get("sentiment").and_then(|v| v.as_str()).unwrap_or("");
            println!("[respond] Finalizing response (sentiment: {})...", sentiment);
            Ok(NodeOutput::new().with_update(
                "response",
                json!(format!("[{}] {}", sentiment.to_uppercase(), response)),
            ))
        })
        // Edges
        .add_edge(START, "classify")
        // Conditional routing based on sentiment
        .add_conditional_edges(
            "classify",
            Router::by_field("sentiment"),
            [
                ("positive", "positive"),
                ("negative", "negative"),
                ("neutral", "neutral"),
            ],
        )
        // All paths lead to respond
        .add_edge("positive", "respond")
        .add_edge("negative", "respond")
        .add_edge("neutral", "respond")
        .add_edge("respond", END)
        .compile()?;

    // Test with different sentiment messages
    let test_messages = [
        "This product is amazing! I love it!",
        "The service was terrible and I'm angry.",
        "I received my order today.",
        "Great job on the new features!",
        "I'm having issues with my account.",
    ];

    for (i, message) in test_messages.iter().enumerate() {
        println!("\n--- Testing: \"{}\" ---\n", message);

        let mut input = State::new();
        input.insert("message".to_string(), json!(message));

        let result = graph.invoke(input, ExecutionConfig::new(&format!("test-{}", i))).await?;

        println!(
            "\nResult: {}\n",
            result.get("response").and_then(|v| v.as_str()).unwrap_or("No response")
        );
        println!("{}", "=".repeat(60));
    }

    println!("\n=== Complete ===");
    Ok(())
}
