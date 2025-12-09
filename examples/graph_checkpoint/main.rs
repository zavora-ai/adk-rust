//! Checkpointing and Persistence with LLM Processing
//!
//! This example demonstrates state persistence with checkpointing using AgentNode
//! for LLM-based processing, enabling:
//! - State recovery after failures
//! - Time travel (viewing/restoring past states)
//! - Long-running workflows that survive restarts
//!
//! Run with: cargo run --example graph_checkpoint
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    checkpoint::{Checkpointer, MemoryCheckpointer},
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
    println!("=== Checkpointing with LLM Processing ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_checkpoint");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create a checkpointer to persist state
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // Create LLM agents for each processing step
    let extractor_agent = Arc::new(
        LlmAgentBuilder::new("extractor")
            .description("Extracts key points from text")
            .model(model.clone())
            .instruction("Extract the 3 most important points from the input text. Be concise.")
            .build()?,
    );

    let analyzer_agent = Arc::new(
        LlmAgentBuilder::new("analyzer")
            .description("Analyzes extracted points")
            .model(model.clone())
            .instruction(
                "Analyze the extracted points and identify themes and sentiment. Be brief.",
            )
            .build()?,
    );

    let summarizer_agent = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .description("Creates final summary")
            .model(model.clone())
            .instruction("Create a one-paragraph executive summary from the analysis.")
            .build()?,
    );

    // Create AgentNodes
    let extractor_node = AgentNode::new(extractor_agent)
        .with_input_mapper(|state| {
            let text = state.get("text").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(&format!("Extract key points: {}", text))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("key_points".to_string(), json!(text));
                        updates.insert("step".to_string(), json!("extraction_complete"));
                    }
                }
            }
            updates
        });

    let analyzer_node = AgentNode::new(analyzer_agent)
        .with_input_mapper(|state| {
            let points = state.get("key_points").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(&format!("Analyze these points:\n{}", points))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("analysis".to_string(), json!(text));
                        updates.insert("step".to_string(), json!("analysis_complete"));
                    }
                }
            }
            updates
        });

    let summarizer_node = AgentNode::new(summarizer_agent)
        .with_input_mapper(|state| {
            let analysis = state.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(&format!("Summarize:\n{}", analysis))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("summary".to_string(), json!(text));
                        updates.insert("step".to_string(), json!("complete"));
                    }
                }
            }
            updates
        });

    // Build a multi-step workflow with checkpointing
    let graph = StateGraph::with_channels(&["text", "key_points", "analysis", "summary", "step"])
        .add_node(extractor_node)
        .add_node(analyzer_node)
        .add_node(summarizer_node)
        .add_edge(START, "extractor")
        .add_edge("extractor", "analyzer")
        .add_edge("analyzer", "summarizer")
        .add_edge("summarizer", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // ========== Part 1: Run workflow with checkpointing ==========
    println!("{}", "=".repeat(60));
    println!("PART 1: Running LLM workflow with checkpointing");
    println!("{}", "=".repeat(60));

    let sample_text = "Artificial intelligence is transforming healthcare through \
        improved diagnostics, personalized treatment plans, and drug discovery. \
        Machine learning models can now detect diseases from medical images with \
        accuracy matching expert radiologists. However, concerns about data privacy \
        and algorithmic bias remain significant challenges.";

    let thread_id = "analysis-pipeline-001";
    let mut input = State::new();
    input.insert("text".to_string(), json!(sample_text));

    println!("\nInput text: \"{}...\"\n", &sample_text[..80]);

    println!("[extractor] Running...");
    println!("[analyzer] Running...");
    println!("[summarizer] Running...");

    let result = graph.invoke(input, ExecutionConfig::new(thread_id)).await?;

    println!("\n--- Results ---");
    println!(
        "\nKey Points:\n{}",
        result.get("key_points").and_then(|v| v.as_str()).unwrap_or("N/A")
    );
    println!("\nAnalysis:\n{}", result.get("analysis").and_then(|v| v.as_str()).unwrap_or("N/A"));
    println!("\nSummary:\n{}", result.get("summary").and_then(|v| v.as_str()).unwrap_or("N/A"));

    // ========== Part 2: View checkpoint history ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 2: Viewing checkpoint history (time travel)");
    println!("{}", "=".repeat(60));

    let checkpoints = checkpointer.list(thread_id).await?;
    println!("\nFound {} checkpoints for thread '{}':", checkpoints.len(), thread_id);

    for (i, cp) in checkpoints.iter().enumerate() {
        let step_name = cp.state.get("step").and_then(|v| v.as_str()).unwrap_or("initial");
        let has_points = cp.state.get("key_points").is_some();
        let has_analysis = cp.state.get("analysis").is_some();
        let has_summary = cp.state.get("summary").is_some();

        println!(
            "  {}. Step {} - {} | points:{} analysis:{} summary:{} | ID: {}...{}",
            i + 1,
            cp.step,
            step_name,
            if has_points { "✓" } else { "✗" },
            if has_analysis { "✓" } else { "✗" },
            if has_summary { "✓" } else { "✗" },
            &cp.checkpoint_id[..8],
            &cp.checkpoint_id[cp.checkpoint_id.len() - 4..]
        );
    }

    // ========== Part 3: Load specific checkpoint ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 3: Loading a specific checkpoint");
    println!("{}", "=".repeat(60));

    if checkpoints.len() >= 2 {
        let checkpoint = &checkpoints[1];
        println!("\nLoading checkpoint after extraction: {}", &checkpoint.checkpoint_id[..16]);

        if let Some(loaded) = checkpointer.load_by_id(&checkpoint.checkpoint_id).await? {
            println!("Checkpoint state at step {}:", loaded.step);
            println!("  - Step: {:?}", loaded.state.get("step").and_then(|v| v.as_str()));
            println!("  - Has key_points: {}", loaded.state.get("key_points").is_some());
            println!("  - Has analysis: {}", loaded.state.get("analysis").is_some());
        }
    }

    // ========== Part 4: Multiple independent threads ==========
    println!("\n{}", "=".repeat(60));
    println!("PART 4: Multiple independent analysis threads");
    println!("{}", "=".repeat(60));

    let texts = [
        ("thread-climate", "Climate change is causing rising sea levels and extreme weather events globally."),
        ("thread-economy", "The global economy is recovering from pandemic disruptions with supply chain improvements."),
    ];

    for (thread, text) in &texts {
        let mut input = State::new();
        input.insert("text".to_string(), json!(text));

        let result = graph.invoke(input, ExecutionConfig::new(thread)).await?;
        println!(
            "\n{}: {}",
            thread,
            result.get("summary").and_then(|v| v.as_str()).unwrap_or("N/A")
        );
    }

    // Show checkpoints per thread
    println!("\nCheckpoints by thread:");
    for (thread, _) in &texts {
        let cps = checkpointer.list(thread).await?;
        println!("  {}: {} checkpoint(s)", thread, cps.len());
    }

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - AgentNode with LLM agents in checkpointed workflow");
    println!("  - Automatic state persistence after each step");
    println!("  - Checkpoint history and time travel debugging");
    println!("  - Multiple independent execution threads");
    Ok(())
}
