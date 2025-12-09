//! GraphAgent with AgentNode and Callbacks
//!
//! This example demonstrates using GraphAgent::builder() to create a graph
//! that implements the ADK Agent trait, with AgentNode for LLM processing
//! and before/after callbacks.
//!
//! Run with: cargo run --example graph_agent
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_core::Agent;
use adk_graph::{
    agent::GraphAgent,
    edge::{END, START},
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== GraphAgent with AgentNode and Callbacks ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_agent");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create specialized LLM agents
    let translator_agent = Arc::new(
        LlmAgentBuilder::new("translator")
            .description("Translates text to French")
            .model(model.clone())
            .instruction("Translate the input text to French. Only output the translation.")
            .build()?,
    );

    let summarizer_agent = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .description("Summarizes text")
            .model(model.clone())
            .instruction("Summarize the input text in one sentence.")
            .build()?,
    );

    // Create AgentNodes
    let translator_node = AgentNode::new(translator_agent)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("translation".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let summarizer_node = AgentNode::new(summarizer_agent)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(text)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("summary".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build a GraphAgent using the builder pattern with callbacks
    let agent = GraphAgent::builder("text_processor")
        .description("Processes text with translation and summarization in parallel")
        .channels(&["input", "translation", "summary", "result"])
        // Add AgentNodes
        .node(translator_node)
        .node(summarizer_node)
        // Combine results
        .node_fn("combine", |ctx| async move {
            let translation = ctx.get("translation").and_then(|v| v.as_str()).unwrap_or("N/A");
            let summary = ctx.get("summary").and_then(|v| v.as_str()).unwrap_or("N/A");

            let result = format!(
                "=== Processing Complete ===\n\n\
                French Translation:\n{}\n\n\
                Summary:\n{}",
                translation, summary
            );

            println!("[combine] Results merged");
            Ok(NodeOutput::new().with_update("result", json!(result)))
        })
        // Parallel execution: both translator and summarizer start from input
        .edge(START, "translator")
        .edge(START, "summarizer")
        .edge("translator", "combine")
        .edge("summarizer", "combine")
        .edge("combine", END)
        // Add callbacks
        .before_agent_callback(|ctx| async move {
            println!(
                "[callback:before] Starting graph execution for session: {}",
                ctx.session_id()
            );
            Ok(())
        })
        .after_agent_callback(|_ctx, event| async move {
            if let Some(content) = event.content() {
                let preview: String = content
                    .parts
                    .iter()
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("")
                    .chars()
                    .take(50)
                    .collect();
                println!("[callback:after] Event received: {}...", preview);
            }
            Ok(())
        })
        .build()?;

    // Use the Agent trait methods
    println!("Agent name: {}", agent.name());
    println!("Agent description: {}", agent.description());
    println!();

    // Test input
    let sample_text = "Artificial intelligence is revolutionizing how we work and live. \
        From healthcare diagnostics to autonomous vehicles, AI systems are becoming \
        integral to modern society. The technology continues to advance rapidly.";

    let mut input = State::new();
    input.insert("input".to_string(), json!(sample_text));

    println!("Input text:\n\"{}\"\n", sample_text);
    println!("{}\n", "=".repeat(60));

    println!("[translator] Processing...");
    println!("[summarizer] Processing...");

    // Execute using GraphAgent's invoke method
    let result = agent.invoke(input, ExecutionConfig::new("processor-thread")).await?;

    println!("\n{}", "=".repeat(60));
    println!("\n{}", result.get("result").and_then(|v| v.as_str()).unwrap_or("No result"));

    // ========== Part 2: Using as ADK Agent ==========
    println!("\n{}", "=".repeat(60));
    println!("Part 2: GraphAgent implements ADK Agent trait");
    println!("{}", "=".repeat(60));

    // Demonstrate that GraphAgent can be used anywhere Agent is expected
    fn describe_agent(a: &dyn Agent) {
        println!("Agent: {} - {}", a.name(), a.description());
        println!("Sub-agents: {}", a.sub_agents().len());
    }

    describe_agent(&agent);

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - GraphAgent::builder() with AgentNode for LLM processing");
    println!("  - Parallel execution (translator + summarizer)");
    println!("  - before_agent_callback and after_agent_callback");
    println!("  - GraphAgent implements ADK Agent trait");
    println!("  - Custom input/output mappers for state transformation");
    Ok(())
}
