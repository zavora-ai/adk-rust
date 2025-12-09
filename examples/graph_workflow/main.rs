//! Graph Workflow with AgentNode
//!
//! This example demonstrates using AgentNode to wrap LlmAgent instances
//! as graph nodes, creating a multi-agent pipeline.
//!
//! Graph: START -> extractor -> analyzer -> formatter -> END
//!
//! Run with: cargo run --example graph_workflow
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Graph Workflow with AgentNode ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_workflow");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create specialized LlmAgent instances
    let extractor_agent = Arc::new(
        LlmAgentBuilder::new("extractor")
            .description("Extracts key entities from text")
            .model(model.clone())
            .instruction(
                "You are an entity extraction specialist. Extract key entities \
                (people, places, organizations, concepts) from the input text. \
                Return them as a structured list.",
            )
            .build()?,
    );

    let analyzer_agent = Arc::new(
        LlmAgentBuilder::new("analyzer")
            .description("Analyzes sentiment and themes")
            .model(model.clone())
            .instruction(
                "You are a text analysis specialist. Analyze the sentiment \
                (positive/negative/neutral) and identify main themes from the input. \
                Provide insights about the content.",
            )
            .build()?,
    );

    let formatter_agent = Arc::new(
        LlmAgentBuilder::new("formatter")
            .description("Formats final summary")
            .model(model.clone())
            .instruction(
                "You are a report formatting specialist. Take the analysis provided \
                and create a professional executive summary with clear sections.",
            )
            .build()?,
    );

    // Wrap agents as graph nodes with custom input/output mappers
    let extractor_node = AgentNode::new(extractor_agent)
        .with_input_mapper(|state| {
            let text = state.get("text").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Extract entities from: {}", text))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("entities".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let analyzer_node = AgentNode::new(analyzer_agent)
        .with_input_mapper(|state| {
            let text = state.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let entities = state.get("entities").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user")
                .with_text(format!("Analyze this text:\n{}\n\nEntities found: {}", text, entities))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("analysis".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let formatter_node = AgentNode::new(formatter_agent)
        .with_input_mapper(|state| {
            let analysis = state.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user")
                .with_text(format!("Format this analysis as an executive summary:\n{}", analysis))
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

    // Build the graph with AgentNodes
    let graph = StateGraph::with_channels(&["text", "entities", "analysis", "summary"])
        .add_node(extractor_node)
        .add_node(analyzer_node)
        .add_node(formatter_node)
        .add_edge(START, "extractor")
        .add_edge("extractor", "analyzer")
        .add_edge("analyzer", "formatter")
        .add_edge("formatter", END)
        .compile()?;

    // Test with sample text
    let sample_text = "OpenAI announced GPT-4 Turbo at their DevDay conference in San Francisco. \
        CEO Sam Altman presented the new model, highlighting its improved context window of 128K tokens \
        and reduced pricing. Microsoft, a major investor in OpenAI, has already integrated the model \
        into Azure. The AI community responded enthusiastically, with many developers praising the \
        cost reductions and new features like JSON mode and improved function calling.";

    let mut input = State::new();
    input.insert("text".to_string(), json!(sample_text));

    println!("Input text:\n\"{}\"\n", sample_text);
    println!("{}\n", "=".repeat(60));

    println!("[Running extractor agent...]");
    println!("[Running analyzer agent...]");
    println!("[Running formatter agent...]");

    let result = graph.invoke(input, ExecutionConfig::new("workflow-thread")).await?;

    println!("\n{}", "=".repeat(60));
    println!("\nEntities:\n{}", result.get("entities").and_then(|v| v.as_str()).unwrap_or("None"));
    println!("\nAnalysis:\n{}", result.get("analysis").and_then(|v| v.as_str()).unwrap_or("None"));
    println!(
        "\nFinal Summary:\n{}",
        result.get("summary").and_then(|v| v.as_str()).unwrap_or("None")
    );

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - Using AgentNode to wrap LlmAgent as graph nodes");
    println!("  - Custom input/output mappers for state transformation");
    println!("  - Multi-agent pipeline with sequential execution");
    Ok(())
}
