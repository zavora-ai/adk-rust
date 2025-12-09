//! Conditional Routing with AgentNode and LLM Classification
//!
//! This example demonstrates conditional edge routing using an LLM classifier
//! agent to determine sentiment, then routing to specialized response agents.
//!
//! Graph: START -> classifier -> [positive | negative | neutral] -> respond -> END
//!
//! Run with: cargo run --example graph_conditional
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    edge::{Router, END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Conditional Routing with AgentNode ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_conditional");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create classifier agent
    let classifier_agent = Arc::new(
        LlmAgentBuilder::new("classifier")
            .description("Classifies text sentiment")
            .model(model.clone())
            .instruction(
                "You are a sentiment classifier. Analyze the input text and respond with \
                ONLY one word: 'positive', 'negative', or 'neutral'. Nothing else.",
            )
            .build()?,
    );

    // Create response agents for each sentiment
    let positive_agent = Arc::new(
        LlmAgentBuilder::new("positive")
            .description("Handles positive feedback")
            .model(model.clone())
            .instruction(
                "You are a customer success specialist. The customer has positive feedback. \
                Express gratitude, reinforce the positive experience, and suggest ways to \
                share their experience. Be warm and appreciative. Keep response under 3 sentences.",
            )
            .build()?,
    );

    let negative_agent = Arc::new(
        LlmAgentBuilder::new("negative")
            .description("Handles negative feedback")
            .model(model.clone())
            .instruction(
                "You are a customer support specialist. The customer has a complaint. \
                Acknowledge their frustration, apologize sincerely, and offer help. \
                Be empathetic. Keep response under 3 sentences.",
            )
            .build()?,
    );

    let neutral_agent = Arc::new(
        LlmAgentBuilder::new("neutral")
            .description("Handles neutral inquiries")
            .model(model.clone())
            .instruction(
                "You are a helpful assistant. The customer has a neutral inquiry. \
                Provide helpful information and offer assistance. Be professional. \
                Keep response under 3 sentences.",
            )
            .build()?,
    );

    // Create AgentNodes
    let classifier_node = AgentNode::new(classifier_agent)
        .with_input_mapper(|state| {
            let text = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Classify this: {}", text))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content
                        .parts
                        .iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("")
                        .to_lowercase();

                    let sentiment = if text.contains("positive") {
                        "positive"
                    } else if text.contains("negative") {
                        "negative"
                    } else {
                        "neutral"
                    };

                    println!("[classifier] Detected sentiment: {}", sentiment);
                    updates.insert("sentiment".to_string(), json!(sentiment));
                }
            }
            updates
        });

    let positive_node = AgentNode::new(positive_agent)
        .with_input_mapper(|state| {
            let msg = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Customer feedback: {}", msg))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        println!("[positive] Generated response");
                        updates.insert("response".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let negative_node = AgentNode::new(negative_agent)
        .with_input_mapper(|state| {
            let msg = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Customer complaint: {}", msg))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        println!("[negative] Generated response");
                        updates.insert("response".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let neutral_node = AgentNode::new(neutral_agent)
        .with_input_mapper(|state| {
            let msg = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Customer inquiry: {}", msg))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        println!("[neutral] Generated response");
                        updates.insert("response".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build the conditional routing graph
    let graph = StateGraph::with_channels(&["message", "sentiment", "response"])
        .add_node(classifier_node)
        .add_node(positive_node)
        .add_node(negative_node)
        .add_node(neutral_node)
        // Final formatting node
        .add_node_fn("respond", |ctx| async move {
            let sentiment = ctx.get("sentiment").and_then(|v| v.as_str()).unwrap_or("");
            let response = ctx.get("response").and_then(|v| v.as_str()).unwrap_or("");
            println!("[respond] Formatting final response");
            let formatted = format!("[{}] {}", sentiment.to_uppercase(), response);
            Ok(NodeOutput::new().with_update("response", json!(formatted)))
        })
        .add_edge(START, "classifier")
        .add_conditional_edges(
            "classifier",
            Router::by_field("sentiment"),
            [("positive", "positive"), ("negative", "negative"), ("neutral", "neutral")],
        )
        .add_edge("positive", "respond")
        .add_edge("negative", "respond")
        .add_edge("neutral", "respond")
        .add_edge("respond", END)
        .compile()?;

    // Test with different sentiments
    let test_messages = [
        "This product is amazing! I love it so much!",
        "The service was terrible and I'm very disappointed.",
        "I received my order today. When does shipping usually take?",
        "Your team went above and beyond. Thank you!",
        "I've been waiting for a refund for weeks. This is unacceptable.",
    ];

    for (i, message) in test_messages.iter().enumerate() {
        println!("\n{}", "=".repeat(70));
        println!("Input: \"{}\"\n", message);

        let mut input = State::new();
        input.insert("message".to_string(), json!(message));

        let result = graph.invoke(input, ExecutionConfig::new(&format!("test-{}", i))).await?;

        println!(
            "\nResponse:\n{}",
            result.get("response").and_then(|v| v.as_str()).unwrap_or("No response")
        );
    }

    println!("\n{}", "=".repeat(70));
    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - LLM-based sentiment classification with AgentNode");
    println!("  - Conditional routing using Router::by_field");
    println!("  - Specialized response agents for different sentiments");
    println!("  - Multi-path graph with convergence");
    Ok(())
}
