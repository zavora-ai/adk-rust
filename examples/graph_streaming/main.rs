//! Graph Streaming Example
//!
//! Demonstrates real-time streaming of LLM responses through adk-graph.
//!
//! Run with:
//!   cargo run --example graph_streaming           # streaming mode (default)
//!   cargo run --example graph_streaming -- --no-stream  # non-streaming mode

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    agent::GraphAgent,
    edge::{END, START},
    node::{AgentNode, ExecutionConfig},
    state::State,
    stream::{StreamEvent, StreamMode},
};
use adk_model::GeminiModel;
use anyhow::Result;
use futures::StreamExt;
use serde_json::json;
use std::io::Write;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let streaming = !std::env::args().any(|a| a == "--no-stream");

    println!("🎭 Graph {} Demo", if streaming { "Streaming" } else { "Non-Streaming" });
    println!("========================\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            return Ok(());
        }
    };

    // Create model and agent
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    let agent = Arc::new(
        LlmAgentBuilder::new("storyteller")
            .model(model)
            .instruction("You are a creative storyteller. Tell engaging stories.")
            .build()?,
    );

    // Create agent node
    let storyteller_node = AgentNode::new(agent)
        .with_input_mapper(|state| {
            let msg = state.get("message").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(msg)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let text: String = events
                .iter()
                .filter_map(|event| event.content())
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| part.text())
                .collect();
            if !text.is_empty() {
                updates.insert("response".to_string(), json!(text));
            }
            updates
        });

    // Build graph
    let graph = GraphAgent::builder("streaming_demo")
        .channels(&["message", "response"])
        .node(storyteller_node)
        .edge(START, "storyteller")
        .edge("storyteller", END)
        .build()?;

    // Prepare input
    let mut state = State::new();
    state.insert("message".to_string(), json!("Tell me a very short story about a robot."));

    println!("📝 Prompt: Tell me a very short story about a robot.\n");

    let config = ExecutionConfig::new("demo-session");

    // Choose streaming mode
    let mode = if streaming { StreamMode::Messages } else { StreamMode::Values };
    let stream = graph.stream(state, config, mode);
    tokio::pin!(stream);

    let mut chunk_count = 0;

    if streaming {
        println!("📖 Response (streaming):\n");
    }

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::NodeStart { node, .. }) => {
                println!("▶ Agent '{}' started\n", node);
            }
            Ok(StreamEvent::Message { content, .. }) if streaming => {
                print!("{}", content);
                std::io::stdout().flush()?;
                chunk_count += 1;
            }
            Ok(StreamEvent::NodeEnd { node, duration_ms, .. }) => {
                println!("\n\n✅ Agent '{}' completed in {}ms", node, duration_ms);
            }
            Ok(StreamEvent::State { state, .. }) if !streaming => {
                if let Some(resp) = state.get("response").and_then(|v| v.as_str()) {
                    println!("📖 Response (complete):\n\n{}", resp);
                }
            }
            Ok(StreamEvent::Done { .. }) => {
                println!("🏁 Done");
            }
            Err(e) => eprintln!("❌ Error: {}", e),
            _ => {}
        }
    }

    if streaming {
        println!("\n📊 {} chunks streamed", chunk_count);
    }

    Ok(())
}
