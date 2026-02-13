use adk_agent::LlmAgentBuilder;
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
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("🚀 Starting Parallel Processing Example");
    println!("This demonstrates translation and summarization running in parallel\n");

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

    // Wrap agents as graph nodes with input/output mappers
    let translator_node = AgentNode::new(translator_agent)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            println!("📝 Translator processing: {}", text);
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
                    if !text.is_empty() {
                        println!("🇫🇷 Translation complete: {}", text);
                        updates.insert("translation".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let summarizer_node = AgentNode::new(summarizer_agent)
        .with_input_mapper(|state| {
            let text = state.get("input").and_then(|v| v.as_str()).unwrap_or("");
            println!("📋 Summarizer processing: {}", text);
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
                    if !text.is_empty() {
                        println!("📝 Summary complete: {}", text);
                        updates.insert("summary".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build the graph with parallel execution
    let agent = GraphAgent::builder("text_processor")
        .description("Processes text with translation and summarization in parallel")
        .channels(&["input", "translation", "summary", "result"])
        .node(translator_node)
        .node(summarizer_node)
        .node_fn("combine", |ctx| async move {
            println!("🔄 Combining results...");
            let translation = ctx.get("translation").and_then(|v| v.as_str()).unwrap_or("N/A");
            let summary = ctx.get("summary").and_then(|v| v.as_str()).unwrap_or("N/A");

            let result = format!(
                "=== Processing Complete ===\n\n\
                French Translation:\n{}\n\n\
                Summary:\n{}",
                translation, summary
            );

            Ok(NodeOutput::new().with_update("result", json!(result)))
        })
        // Parallel execution: both nodes start simultaneously
        .edge(START, "translator")
        .edge(START, "summarizer")
        .edge("translator", "combine")
        .edge("summarizer", "combine")
        .edge("combine", END)
        .build()?;

    // Execute the graph
    let mut input = State::new();
    input.insert("input".to_string(), json!("AI is transforming how we work and live."));

    println!("⚡ Executing parallel processing...\n");
    let result = agent.invoke(input, ExecutionConfig::new("thread-1")).await?;
    
    println!("\n✅ Final Result:");
    println!("{}", result.get("result").and_then(|v| v.as_str()).unwrap_or(""));

    Ok(())
}
