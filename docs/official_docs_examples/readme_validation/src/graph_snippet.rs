//! README Graph-Based Workflows snippet validation

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    node::{AgentNode, ExecutionConfig, NodeOutput},
    prelude::*,
};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY").unwrap_or_default();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Create LLM agents for different tasks
    let translator = Arc::new(
        LlmAgentBuilder::new("translator")
            .model(model.clone())
            .instruction("Translate the input text to French.")
            .build()?,
    );

    let summarizer = Arc::new(
        LlmAgentBuilder::new("summarizer")
            .model(model.clone())
            .instruction("Summarize the input text in one sentence.")
            .build()?,
    );

    // Create AgentNodes with custom input/output mappers
    let translator_node = AgentNode::new(translator)
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
                    updates.insert("translation".to_string(), json!(text));
                }
            }
            updates
        });

    let summarizer_node = AgentNode::new(summarizer)
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
                    updates.insert("summary".to_string(), json!(text));
                }
            }
            updates
        });

    // Build graph with parallel execution
    let _agent = GraphAgent::builder("text_processor")
        .description("Translates and summarizes text in parallel")
        .channels(&["input", "translation", "summary"])
        .node(translator_node)
        .node(summarizer_node)
        .node_fn("combine", |ctx| async move {
            let t = ctx.get("translation").and_then(|v| v.as_str()).unwrap_or("");
            let s = ctx.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            Ok(NodeOutput::new()
                .with_update("result", json!(format!("Translation: {}\nSummary: {}", t, s))))
        })
        .edge(START, "translator")
        .edge(START, "summarizer") // Parallel execution
        .edge("translator", "combine")
        .edge("summarizer", "combine")
        .edge("combine", END)
        .build()?;

    // Execute
    let mut input = State::new();
    input.insert("input".to_string(), json!("AI is transforming how we work."));
    let _result = _agent.invoke(input, ExecutionConfig::new("thread-1")).await?;

    println!("✓ Graph snippet compiles");
    Ok(())
}
