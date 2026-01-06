use adk_agent::LlmAgentBuilder;
use adk_graph::{
    checkpoint::{SqliteCheckpointer, Checkpointer},
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    println!("💾 Starting Checkpointing Example");
    println!("This demonstrates state persistence and time travel debugging\n");

    // Create SQLite checkpointer (in-memory for demo)
    let checkpointer = Arc::new(SqliteCheckpointer::new("sqlite::memory:").await?);
    println!("📁 Created in-memory SQLite checkpointer");

    // Create processing agents
    let analyzer_agent = Arc::new(
        LlmAgentBuilder::new("analyzer")
            .description("Analyzes input data")
            .model(model.clone())
            .instruction("Analyze the input and provide key insights. Be concise.")
            .build()?,
    );

    let processor_agent = Arc::new(
        LlmAgentBuilder::new("processor")
            .description("Processes analyzed data")
            .model(model.clone())
            .instruction("Process the analysis and create actionable recommendations.")
            .build()?,
    );

    let finalizer_agent = Arc::new(
        LlmAgentBuilder::new("finalizer")
            .description("Finalizes the output")
            .model(model.clone())
            .instruction("Create a final summary combining analysis and recommendations.")
            .build()?,
    );

    // Create nodes with checkpoint logging
    let analyzer_node = AgentNode::new(analyzer_agent)
        .with_input_mapper(|state| {
            let data = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            println!("🔍 Step 1: Analyzing data...");
            adk_core::Content::new("user").with_text(data)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    println!("📊 Analysis complete: {}", text.chars().take(50).collect::<String>() + "...");
                    updates.insert("analysis".to_string(), json!(text));
                }
            }
            updates
        });

    let processor_node = AgentNode::new(processor_agent)
        .with_input_mapper(|state| {
            let analysis = state.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
            println!("⚙️  Step 2: Processing analysis...");
            adk_core::Content::new("user").with_text(&format!("Based on this analysis: {}", analysis))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    println!("🔧 Processing complete: {}", text.chars().take(50).collect::<String>() + "...");
                    updates.insert("recommendations".to_string(), json!(text));
                }
            }
            updates
        });

    let finalizer_node = AgentNode::new(finalizer_agent)
        .with_input_mapper(|state| {
            let analysis = state.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
            let recommendations = state.get("recommendations").and_then(|v| v.as_str()).unwrap_or("");
            println!("📝 Step 3: Creating final summary...");
            adk_core::Content::new("user").with_text(&format!(
                "Create a final summary from:\nAnalysis: {}\nRecommendations: {}", 
                analysis, recommendations
            ))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    println!("✅ Final summary complete");
                    updates.insert("final_result".to_string(), json!(text));
                }
            }
            updates
        });

    // Build graph with checkpointing
    let graph = StateGraph::with_channels(&["input", "analysis", "recommendations", "final_result", "step"])
        .add_node(analyzer_node)
        .add_node(processor_node)
        .add_node(finalizer_node)
        .add_node_fn("step_counter", |ctx| async move {
            let step = ctx.get("step").and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            println!("📍 Checkpoint: Step {}", step);
            Ok(NodeOutput::new().with_update("step", json!(step)))
        })
        .add_edge(START, "step_counter")
        .add_edge("step_counter", "analyzer")
        .add_edge("analyzer", "step_counter")
        .add_edge("step_counter", "processor")
        .add_edge("processor", "step_counter")
        .add_edge("step_counter", "finalizer")
        .add_edge("finalizer", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // Execute workflow with checkpointing
    let thread_id = "workflow-demo";
    let mut input = State::new();
    input.insert("input".to_string(), json!("Analyze the impact of remote work on team productivity"));

    println!("🚀 Starting workflow with checkpointing...\n");
    let result = graph.invoke(input, ExecutionConfig::new(thread_id)).await?;

    println!("\n🎯 Workflow Complete!");
    println!("Final result: {}", result.get("final_result").and_then(|v| v.as_str()).unwrap_or("N/A"));

    // Demonstrate checkpoint history (time travel)
    println!("\n🕰️  Checkpoint History (Time Travel):");
    let checkpoints = checkpointer.list(thread_id).await?;
    
    for (i, checkpoint) in checkpoints.iter().enumerate() {
        let step = checkpoint.state.get("step").and_then(|v| v.as_i64()).unwrap_or(0);
        let has_analysis = checkpoint.state.contains_key("analysis");
        let has_recommendations = checkpoint.state.contains_key("recommendations");
        let has_final = checkpoint.state.contains_key("final_result");
        
        let status = if has_final { "✅ Complete" }
            else if has_recommendations { "🔧 Processing" }
            else if has_analysis { "📊 Analyzed" }
            else { "🔍 Starting" };
        
        println!("  {}. Step {} - {} (ID: {})", 
            i + 1, step, status, &checkpoint.checkpoint_id[..8]);
    }

    // Demonstrate loading a specific checkpoint
    if let Some(checkpoint) = checkpoints.get(1) {
        println!("\n🔄 Loading checkpoint from step {}...", 
            checkpoint.state.get("step").and_then(|v| v.as_i64()).unwrap_or(0));
        
        let loaded_checkpoint = checkpointer.load_by_id(&checkpoint.checkpoint_id).await?;
        if let Some(cp) = loaded_checkpoint {
            println!("📋 Checkpoint state:");
            println!("  - Has analysis: {}", cp.state.contains_key("analysis"));
            println!("  - Has recommendations: {}", cp.state.contains_key("recommendations"));
            println!("  - Has final result: {}", cp.state.contains_key("final_result"));
        }
    }

    // Clean up (no file to remove for in-memory DB)
    println!("\n✅ Checkpointing demonstration complete!");

    Ok(())
}
