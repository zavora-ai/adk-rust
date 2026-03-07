//! Graph Workflow with OpenAI
//!
//! This example demonstrates using OpenAI GPT within graph nodes
//! to create an AI-powered Q&A system with classification and routing.
//!
//! Graph: START -> classify -> [technical | general | creative] -> respond -> END

use adk_core::{Content, Llm, LlmRequest};
use adk_graph::{
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;

/// Helper to call LLM and collect the full response
async fn call_llm(client: &Arc<OpenAIClient>, prompt: &str) -> Result<String, GraphError> {
    let request = LlmRequest::new(client.name(), vec![Content::new("user").with_text(prompt)]);

    let mut stream = client.generate_content(request, false).await.map_err(|e| {
        GraphError::NodeExecutionFailed { node: "llm".to_string(), message: e.to_string() }
    })?;

    let mut result = String::new();
    while let Some(response_result) = stream.next().await {
        let Ok(response) = response_result else { continue };
        let Some(content) = response.content else { continue };

        for part in content.parts {
            if let Some(text) = part.text() {
                result.push_str(text);
            }
        }
    }

    Ok(result)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Graph Workflow with OpenAI ===\n");

    // Initialize the OpenAI client
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let client = Arc::new(OpenAIClient::new(OpenAIConfig {
        api_key,
        model: "gpt-5-mini".to_string(),
        ..Default::default()
    })?);
    let client_classify = client.clone();
    let client_tech = client.clone();
    let client_general = client.clone();
    let client_creative = client.clone();

    // Build a Q&A system with intelligent routing
    let graph = StateGraph::with_channels(&["question", "category", "answer"])
        // Step 1: Classify the question type
        .add_node_fn("classify", move |ctx| {
            let client = client_classify.clone();
            async move {
                let question = ctx.get("question").and_then(|v| v.as_str()).unwrap_or("");
                println!("[classify] Analyzing question: \"{}\"", question);

                let prompt = format!(
                    "Classify this question into exactly one category. \
                     Reply with only the category name, nothing else.\n\
                     Categories: technical, general, creative\n\n\
                     Question: {}",
                    question
                );

                let response = call_llm(&client, &prompt).await?;
                let category = response.trim().to_lowercase();

                // Normalize to valid categories
                let category = match category.as_str() {
                    "technical" => "technical",
                    "creative" => "creative",
                    _ => "general",
                };

                println!("[classify] Category: {}", category);
                Ok(NodeOutput::new().with_update("category", json!(category)))
            }
        })
        // Technical question handler
        .add_node_fn("technical", move |ctx| {
            let client = client_tech.clone();
            async move {
                let question = ctx.get("question").and_then(|v| v.as_str()).unwrap_or("");
                println!("[technical] Generating technical answer...");

                let prompt = format!(
                    "You are a technical expert. Provide a precise, accurate answer.\n\nQuestion: {}",
                    question
                );

                let answer = call_llm(&client, &prompt).await?;
                Ok(NodeOutput::new().with_update("answer", json!(answer)))
            }
        })
        // General question handler
        .add_node_fn("general", move |ctx| {
            let client = client_general.clone();
            async move {
                let question = ctx.get("question").and_then(|v| v.as_str()).unwrap_or("");
                println!("[general] Generating informative answer...");

                let prompt = format!(
                    "Provide a helpful answer. Be clear and concise.\n\nQuestion: {}",
                    question
                );

                let answer = call_llm(&client, &prompt).await?;
                Ok(NodeOutput::new().with_update("answer", json!(answer)))
            }
        })
        // Creative question handler
        .add_node_fn("creative", move |ctx| {
            let client = client_creative.clone();
            async move {
                let question = ctx.get("question").and_then(|v| v.as_str()).unwrap_or("");
                println!("[creative] Generating creative response...");

                let prompt = format!(
                    "You are a creative writer. Provide an imaginative response.\n\nQuestion: {}",
                    question
                );

                let answer = call_llm(&client, &prompt).await?;
                Ok(NodeOutput::new().with_update("answer", json!(answer)))
            }
        })
        // Final formatting
        .add_node_fn("respond", |ctx| async move {
            let category = ctx.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let answer = ctx.get("answer").and_then(|v| v.as_str()).unwrap_or("");
            println!("[respond] Formatting final response...");

            let formatted = format!("[{}] {}", category.to_uppercase(), answer);
            Ok(NodeOutput::new().with_update("answer", json!(formatted)))
        })
        // Define edges
        .add_edge(START, "classify")
        .add_conditional_edges(
            "classify",
            |state| {
                state
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("general")
                    .to_string()
            },
            [
                ("technical", "technical"),
                ("general", "general"),
                ("creative", "creative"),
            ],
        )
        .add_edge("technical", "respond")
        .add_edge("general", "respond")
        .add_edge("creative", "respond")
        .add_edge("respond", END)
        .compile()?;

    // Test with different types of questions
    let test_questions = [
        "How do I implement a binary search tree in Rust?",
        "What's the capital of France?",
        "Write a short poem about programming",
    ];

    for (i, question) in test_questions.iter().enumerate() {
        println!("\n{}", "=".repeat(60));
        println!("Q: {}\n", question);

        let mut input = State::new();
        input.insert("question".to_string(), json!(question));

        let result = graph.invoke(input, ExecutionConfig::new(format!("qa-{}", i))).await?;

        println!("\nA: {}\n", result.get("answer").and_then(|v| v.as_str()).unwrap_or("No answer"));
    }

    println!("\n=== Complete ===");
    Ok(())
}
