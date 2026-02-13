use adk_agent::LlmAgentBuilder;
use adk_graph::{
    edge::{END, Router, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("🎯 Starting Conditional Routing Example");
    println!("This demonstrates sentiment-based routing to different response handlers\n");

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
            .description("Handles neutral feedback")
            .model(model.clone())
            .instruction(
                "You are a customer service representative. The customer has neutral feedback. \
                Ask clarifying questions to better understand their needs. Be helpful and curious. \
                Keep response under 3 sentences.",
            )
            .build()?,
    );

    // Create AgentNodes with mappers
    let classifier_node = AgentNode::new(classifier_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            println!("🧠 Classifying sentiment for: {}", text);
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("")
                        .to_lowercase()
                        .trim()
                        .to_string();
                    
                    let sentiment = if text.contains("positive") { "positive" }
                        else if text.contains("negative") { "negative" }
                        else { "neutral" };
                    
                    println!("📊 Sentiment detected: {}", sentiment);
                    updates.insert("sentiment".to_string(), json!(sentiment));
                }
            }
            updates
        });

    // Response nodes
    let positive_node = AgentNode::new(positive_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            println!("😊 Generating positive response...");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    updates.insert("response".to_string(), json!(text));
                }
            }
            updates
        });

    let negative_node = AgentNode::new(negative_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            println!("😔 Generating empathetic response...");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    updates.insert("response".to_string(), json!(text));
                }
            }
            updates
        });

    let neutral_node = AgentNode::new(neutral_agent)
        .with_input_mapper(|state| {
            let text = state.get("feedback").and_then(|v| v.as_str()).unwrap_or("");
            println!("😐 Generating clarifying response...");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    updates.insert("response".to_string(), json!(text));
                }
            }
            updates
        });

    // Build graph with conditional routing
    let graph = StateGraph::with_channels(&["feedback", "sentiment", "response"])
        .add_node(classifier_node)
        .add_node(positive_node)
        .add_node(negative_node)
        .add_node(neutral_node)
        .add_edge(START, "classifier")
        .add_conditional_edges(
            "classifier",
            Router::by_field("sentiment"),  // Route based on sentiment field
            [
                ("positive", "positive"),
                ("negative", "negative"),
                ("neutral", "neutral"),
            ],
        )
        .add_edge("positive", END)
        .add_edge("negative", END)
        .add_edge("neutral", END)
        .compile()?;

    // Test with different feedback types
    let test_cases = vec![
        "Your product is amazing! I love it!",
        "This is terrible. I want my money back.",
        "It's okay, I guess. Not sure what to think.",
    ];

    for (i, feedback) in test_cases.iter().enumerate() {
        println!("\n🔄 Test Case {}: {}", i + 1, feedback);
        
        let mut input = State::new();
        input.insert("feedback".to_string(), json!(feedback));

        let result = graph.invoke(input, ExecutionConfig::new(&format!("feedback-{}", i + 1))).await?;
        
        println!("📊 Sentiment: {}", result.get("sentiment").and_then(|v| v.as_str()).unwrap_or(""));
        println!("💬 Response: {}", result.get("response").and_then(|v| v.as_str()).unwrap_or(""));
        println!("{}", "─".repeat(50));
    }

    println!("\n✅ Conditional routing demonstration complete!");

    Ok(())
}
