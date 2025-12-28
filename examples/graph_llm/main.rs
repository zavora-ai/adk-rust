//! Graph Workflow with Gemini LLM
//!
//! This example demonstrates using Gemini LLM within graph nodes
//! to create an AI-powered content generation pipeline.
//!
//! Graph: START -> generate_outline -> write_content -> review -> END

use adk_core::{Content, Llm, LlmRequest};
use adk_graph::{
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;

/// Helper to call LLM and collect the full response
async fn call_llm(
    model: &Arc<GeminiModel>,
    model_name: &str,
    prompt: &str,
) -> Result<String, GraphError> {
    let request = LlmRequest::new(model_name, vec![Content::new("user").with_text(prompt)]);

    let mut stream = model.generate_content(request, false).await.map_err(|e| {
        GraphError::NodeExecutionFailed { node: "llm".to_string(), message: e.to_string() }
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
    println!("=== Graph Workflow with Gemini LLM ===\n");

    // Initialize the model
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model_name = "gemini-2.0-flash";
    let model = Arc::new(GeminiModel::new(&api_key, model_name)?);
    let model1 = model.clone();
    let model2 = model.clone();
    let model3 = model.clone();
    let mn1 = model_name.to_string();
    let mn2 = model_name.to_string();
    let mn3 = model_name.to_string();

    // Build an AI content generation pipeline
    let graph = StateGraph::with_channels(&["topic", "outline", "content", "final"])
        // Step 1: Generate an outline for the topic
        .add_node_fn("generate_outline", move |ctx| {
            let model = model1.clone();
            let model_name = mn1.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("technology");
                println!("[generate_outline] Creating outline for: {}", topic);

                let prompt = format!(
                    "Create a brief outline (3-4 points) for a short article about: {}. \
                     Return only the outline points, one per line.",
                    topic
                );

                let outline = call_llm(&model, &model_name, &prompt).await?;
                println!("[generate_outline] Outline:\n{}\n", outline);
                Ok(NodeOutput::new().with_update("outline", json!(outline)))
            }
        })
        // Step 2: Write content based on outline
        .add_node_fn("write_content", move |ctx| {
            let model = model2.clone();
            let model_name = mn2.clone();
            async move {
                let topic = ctx.get("topic").and_then(|v| v.as_str()).unwrap_or("technology");
                let outline = ctx.get("outline").and_then(|v| v.as_str()).unwrap_or("");
                println!("[write_content] Writing content based on outline...");

                let prompt = format!(
                    "Write a short article (2-3 paragraphs) about {} based on this outline:\n{}\n\n\
                     Keep it concise and informative.",
                    topic, outline
                );

                let content = call_llm(&model, &model_name, &prompt).await?;
                println!("[write_content] Content generated ({} chars)\n", content.len());
                Ok(NodeOutput::new().with_update("content", json!(content)))
            }
        })
        // Step 3: Review and improve the content
        .add_node_fn("review", move |ctx| {
            let model = model3.clone();
            let model_name = mn3.clone();
            async move {
                let content = ctx.get("content").and_then(|v| v.as_str()).unwrap_or("");
                println!("[review] Reviewing and polishing content...");

                let prompt = format!(
                    "Review this article and make it more engaging. \
                     Fix any issues and add a compelling title. Return the improved version:\n\n{}",
                    content
                );

                let final_content = call_llm(&model, &model_name, &prompt).await?;
                println!("[review] Review complete!\n");
                Ok(NodeOutput::new().with_update("final", json!(final_content)))
            }
        })
        // Define the pipeline flow
        .add_edge(START, "generate_outline")
        .add_edge("generate_outline", "write_content")
        .add_edge("write_content", "review")
        .add_edge("review", END)
        .compile()?;

    // Run the pipeline with a topic
    let topic = "The benefits of Rust for systems programming";
    println!("Topic: {}\n", topic);
    println!("{}\n", "=".repeat(60));

    let mut input = State::new();
    input.insert("topic".to_string(), json!(topic));

    let result = graph.invoke(input, ExecutionConfig::new("content-pipeline")).await?;

    println!("{}\n", "=".repeat(60));
    println!("FINAL ARTICLE:\n");
    println!("{}", result.get("final").and_then(|v| v.as_str()).unwrap_or("No content generated"));

    println!("\n=== Complete ===");
    Ok(())
}
