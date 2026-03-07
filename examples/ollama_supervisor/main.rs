//! Ollama Supervisor Multi-Agent Example
//!
//! Demonstrates a supervisor pattern where a coordinator agent routes tasks
//! to specialized worker agents, all running locally via Ollama.
//!
//! Graph: supervisor -> [researcher | writer | coder] -> supervisor (cycle)
//!
//! Run: cargo run --example ollama_supervisor --features ollama

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Ollama Supervisor Multi-Agent Pattern");
    println!("======================================\n");

    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    println!("Using model: {}", model_name);
    println!("Make sure: ollama serve && ollama pull {}\n", model_name);

    let model = Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?);

    // Supervisor agent - decides which worker to use next
    let supervisor_agent = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Routes tasks to specialists")
            .model(model.clone())
            .instruction(
                "You are a task supervisor coordinating a team. You MUST use ALL specialists before finishing.\n\n\
                Available specialists:\n\
                - researcher: Gathers information (use FIRST)\n\
                - writer: Writes content based on research (use SECOND)\n\
                - coder: Writes code examples (use THIRD)\n\n\
                Rules:\n\
                1. If 'researcher' not in Completed list, respond: researcher\n\
                2. If 'writer' not in Completed list, respond: writer\n\
                3. If 'coder' not in Completed list, respond: coder\n\
                4. Only if ALL THREE are completed, respond: done\n\n\
                Respond with ONLY ONE WORD: researcher, writer, coder, or done",
            )
            .build()?,
    );

    // Worker agents
    let researcher_agent = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Research specialist")
            .model(model.clone())
            .instruction("Research the topic briefly. Provide 2-3 key findings.")
            .build()?,
    );

    let writer_agent = Arc::new(
        LlmAgentBuilder::new("writer")
            .description("Content writer")
            .model(model.clone())
            .instruction("Write a brief paragraph based on available research.")
            .build()?,
    );

    let coder_agent = Arc::new(
        LlmAgentBuilder::new("coder")
            .description("Code specialist")
            .model(model.clone())
            .instruction("Write a short code example related to the topic.")
            .build()?,
    );

    // Build AgentNodes with mappers
    let supervisor_node = AgentNode::new(supervisor_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let history =
                state.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let done: Vec<String> =
                history.iter().filter_map(|h| h.as_str().map(|s| s.to_string())).collect();

            adk_core::Content::new("user").with_text(format!(
                "Task: {}\nCompleted: {:?}\nWho next? (researcher/writer/coder/done)",
                task, done
            ))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();

            // Accumulate all text from all events
            let mut full_text = String::new();
            for event in events {
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
            }

            let text = full_text.to_lowercase();
            println!("[supervisor] full response: {:?}", text);

            let next = if text.contains("researcher") {
                "researcher"
            } else if text.contains("writer") {
                "writer"
            } else if text.contains("coder") {
                "coder"
            } else {
                "done"
            };

            println!("[supervisor] routing to: {}", next);
            updates.insert("next_agent".to_string(), json!(next));
            updates
        });

    let researcher_node = AgentNode::new(researcher_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Research: {}", task))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let mut full_text = String::new();
            for event in events {
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
            }
            println!("[researcher] output: {} chars", full_text.len());
            if !full_text.is_empty() {
                updates.insert("research".to_string(), json!(full_text));
            }
            updates
        });

    let writer_node = AgentNode::new(writer_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let research = state.get("research").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user")
                .with_text(format!("Write about: {}\nResearch: {}", task, research))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let mut full_text = String::new();
            for event in events {
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
            }
            println!("[writer] output: {} chars", full_text.len());
            if !full_text.is_empty() {
                updates.insert("content".to_string(), json!(full_text));
            }
            updates
        });

    let coder_node = AgentNode::new(coder_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Code for: {}", task))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            let mut full_text = String::new();
            for event in events {
                if let Some(content) = event.content() {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
            }
            println!("[coder] output: {} chars", full_text.len());
            if !full_text.is_empty() {
                updates.insert("code".to_string(), json!(full_text));
            }
            updates
        });

    // Build the graph
    let graph = StateGraph::with_channels(&[
        "task",
        "next_agent",
        "history",
        "research",
        "content",
        "code",
        "result",
    ])
    .add_node(supervisor_node)
    .add_node(researcher_node)
    .add_node(writer_node)
    .add_node(coder_node)
    .add_node_fn("track_researcher", |ctx| async move {
        let mut h = ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        h.push(json!("researcher"));
        println!("[researcher] done");
        Ok(NodeOutput::new().with_update("history", json!(h)))
    })
    .add_node_fn("track_writer", |ctx| async move {
        let mut h = ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        h.push(json!("writer"));
        println!("[writer] done");
        Ok(NodeOutput::new().with_update("history", json!(h)))
    })
    .add_node_fn("track_coder", |ctx| async move {
        let mut h = ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        h.push(json!("coder"));
        println!("[coder] done");
        Ok(NodeOutput::new().with_update("history", json!(h)))
    })
    .add_node_fn("finalize", |ctx| async move {
        let research = ctx.get("research").and_then(|v| v.as_str()).unwrap_or("N/A");
        let content = ctx.get("content").and_then(|v| v.as_str()).unwrap_or("N/A");
        let code = ctx.get("code").and_then(|v| v.as_str()).unwrap_or("N/A");
        let result = format!(
            "=== RESULT ===\n\nRESEARCH:\n{}\n\nCONTENT:\n{}\n\nCODE:\n{}",
            research, content, code
        );
        Ok(NodeOutput::new().with_update("result", json!(result)))
    })
    .add_edge(START, "supervisor")
    .add_conditional_edges(
        "supervisor",
        |state| state.get("next_agent").and_then(|v| v.as_str()).unwrap_or("done").to_string(),
        [
            ("researcher", "researcher"),
            ("writer", "writer"),
            ("coder", "coder"),
            ("done", "finalize"),
        ],
    )
    .add_edge("researcher", "track_researcher")
    .add_edge("track_researcher", "supervisor")
    .add_edge("writer", "track_writer")
    .add_edge("track_writer", "supervisor")
    .add_edge("coder", "track_coder")
    .add_edge("track_coder", "supervisor")
    .add_edge("finalize", END)
    .compile()?
    .with_recursion_limit(15);

    // Run example
    let task = "Explain Rust ownership in simple terms";
    println!("TASK: \"{}\"\n", task);

    let mut input = State::new();
    input.insert("task".to_string(), json!(task));
    input.insert("history".to_string(), json!([]));

    let result = graph.invoke(input, ExecutionConfig::new("supervisor-thread".to_string())).await?;

    println!("\n{}", result.get("result").and_then(|v| v.as_str()).unwrap_or("No result"));

    Ok(())
}
