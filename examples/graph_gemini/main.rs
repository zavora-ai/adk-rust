//! Graph Workflow with Gemini
//!
//! This example demonstrates using Google's Gemini within graph nodes
//! to create an AI-powered research assistant with multi-step analysis.
//!
//! Graph: START -> analyze -> [summarize | compare | explain] -> format -> END
//!
//! Run with: cargo run --example graph_gemini
//!
//! Requires: GOOGLE_API_KEY environment variable

use adk_core::{Content, Llm, LlmRequest};
use adk_graph::{
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;

/// Helper to call Gemini and collect the full response
async fn call_gemini(model: &Arc<GeminiModel>, prompt: &str) -> Result<String, GraphError> {
    let request = LlmRequest::new(model.name(), vec![Content::new("user").with_text(prompt)]);

    let mut stream = model.generate_content(request, false).await.map_err(|e| {
        GraphError::NodeExecutionFailed { node: "gemini".to_string(), message: e.to_string() }
    })?;

    let mut result = String::new();
    while let Some(response_result) = stream.next().await {
        if let Ok(response) = response_result
            && let Some(content) = response.content
        {
            for part in content.parts {
                if let Some(text) = part.text() {
                    result.push_str(text);
                }
            }
        }
    }

    Ok(result)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Graph Workflow with Gemini ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_gemini");
            return Ok(());
        }
    };

    // Initialize Gemini model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Clone for each node that needs the model
    let model_analyze = model.clone();
    let model_summarize = model.clone();
    let model_compare = model.clone();
    let model_explain = model.clone();

    // Build a research assistant with intelligent task routing
    let graph = StateGraph::with_channels(&["topic", "task_type", "analysis", "result"])
        // Step 1: Analyze the research topic and determine task type
        .add_node_fn("analyze", move |ctx| {
            let model = model_analyze.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                println!("[analyze] Processing topic: \"{}\"", topic);

                let prompt = format!(
                    "Analyze this research topic and classify the best approach. \
                     Reply with exactly one word: summarize, compare, or explain.\n\n\
                     - Use 'summarize' for topics needing a brief overview\n\
                     - Use 'compare' for topics involving multiple items to contrast\n\
                     - Use 'explain' for complex concepts needing detailed explanation\n\n\
                     Topic: {}",
                    topic
                );

                let response = call_gemini(&model, &prompt).await?;
                let task_type = response.trim().to_lowercase();

                // Normalize to valid task types
                let task_type = match task_type.as_str() {
                    t if t.contains("compare") => "compare",
                    t if t.contains("explain") => "explain",
                    _ => "summarize",
                };

                println!("[analyze] Task type: {}", task_type);

                // Also generate initial analysis
                let analysis_prompt = format!(
                    "Briefly identify the key aspects of this topic (2-3 bullet points):\n{}",
                    topic
                );
                let analysis = call_gemini(&model, &analysis_prompt).await?;

                Ok(NodeOutput::new()
                    .with_update("task_type", json!(task_type))
                    .with_update("analysis", json!(analysis)))
            }
        })
        // Summarize handler - brief overview
        .add_node_fn("summarize", move |ctx| {
            let model = model_summarize.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                let analysis = ctx.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
                println!("[summarize] Creating summary...");

                let prompt = format!(
                    "Based on this analysis:\n{}\n\n\
                     Provide a concise 2-3 paragraph summary of: {}",
                    analysis, topic
                );

                let result = call_gemini(&model, &prompt).await?;
                Ok(NodeOutput::new().with_update("result", json!(result)))
            }
        })
        // Compare handler - contrast multiple aspects
        .add_node_fn("compare", move |ctx| {
            let model = model_compare.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                let analysis = ctx.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
                println!("[compare] Creating comparison...");

                let prompt = format!(
                    "Based on this analysis:\n{}\n\n\
                     Create a structured comparison for: {}\n\n\
                     Include: similarities, differences, and a recommendation.",
                    analysis, topic
                );

                let result = call_gemini(&model, &prompt).await?;
                Ok(NodeOutput::new().with_update("result", json!(result)))
            }
        })
        // Explain handler - detailed explanation
        .add_node_fn("explain", move |ctx| {
            let model = model_explain.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("");
                let analysis = ctx.get("analysis").and_then(|v| v.as_str()).unwrap_or("");
                println!("[explain] Creating detailed explanation...");

                let prompt = format!(
                    "Based on this analysis:\n{}\n\n\
                     Provide a clear, educational explanation of: {}\n\n\
                     Include examples and break down complex concepts.",
                    analysis, topic
                );

                let result = call_gemini(&model, &prompt).await?;
                Ok(NodeOutput::new().with_update("result", json!(result)))
            }
        })
        // Final formatting
        .add_node_fn("format", |ctx| async move {
            let task_type = ctx.get("task_type").and_then(|v| v.as_str()).unwrap_or("");
            let result = ctx.get("result").and_then(|v| v.as_str()).unwrap_or("");
            println!("[format] Formatting final output...");

            let header = match task_type {
                "summarize" => "SUMMARY",
                "compare" => "COMPARISON",
                "explain" => "EXPLANATION",
                _ => "RESULT",
            };

            let formatted = format!("=== {} ===\n\n{}", header, result);
            Ok(NodeOutput::new().with_update("result", json!(formatted)))
        })
        // Define edges
        .add_edge(START, "analyze")
        .add_conditional_edges(
            "analyze",
            |state| {
                state.get("task_type").and_then(|v| v.as_str()).unwrap_or("summarize").to_string()
            },
            [("summarize", "summarize"), ("compare", "compare"), ("explain", "explain")],
        )
        .add_edge("summarize", "format")
        .add_edge("compare", "format")
        .add_edge("explain", "format")
        .add_edge("format", END)
        .compile()?;

    // Test with different types of research topics
    let test_topics = [
        "The history and impact of the Rust programming language",
        "Rust vs Go for backend development",
        "How does Rust's ownership system prevent memory bugs",
    ];

    for (i, topic) in test_topics.iter().enumerate() {
        println!("\n{}", "=".repeat(70));
        println!("Topic: {}\n", topic);

        let mut input = State::new();
        input.insert("topic".to_string(), json!(topic));

        let result = graph.invoke(input, ExecutionConfig::new(&format!("research-{}", i))).await?;

        println!("\n{}\n", result.get("result").and_then(|v| v.as_str()).unwrap_or("No result"));
    }

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - Gemini integration in graph nodes");
    println!("  - Multi-step LLM workflows (analyze -> task -> format)");
    println!("  - Conditional routing based on LLM classification");
    println!("  - State passing between nodes");

    Ok(())
}
