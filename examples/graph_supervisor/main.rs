//! Multi-Agent Supervisor Pattern with AgentNode
//!
//! This example demonstrates a supervisor agent that dynamically routes tasks
//! to specialized worker agents (all using real LLMs via AgentNode).
//!
//! Graph:
//!   START -> supervisor -> [researcher | writer | coder | done: finalize] -> supervisor (cycle)
//!
//! Run with: cargo run --example graph_supervisor
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_graph::{
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
    println!("=== Multi-Agent Supervisor Pattern ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_supervisor");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create the supervisor agent
    let supervisor_agent = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .description("Routes tasks to specialized agents")
            .model(model.clone())
            .instruction(
                "You are a task supervisor. Based on the task and work completed so far, \
                decide which specialist should work next.\n\n\
                Available specialists:\n\
                - researcher: For gathering information and research\n\
                - writer: For writing content, documentation, articles\n\
                - coder: For writing code and technical implementation\n\n\
                Respond with ONLY one word: 'researcher', 'writer', 'coder', or 'done' \
                (if all work is complete).\n\n\
                Consider what has already been done and what still needs to be done.",
            )
            .build()?,
    );

    // Create specialized worker agents
    let researcher_agent = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Research specialist")
            .model(model.clone())
            .instruction(
                "You are a research specialist. Gather key information about the topic. \
                Provide findings as bullet points with sources mentioned.",
            )
            .build()?,
    );

    let writer_agent = Arc::new(
        LlmAgentBuilder::new("writer")
            .description("Content writer")
            .model(model.clone())
            .instruction(
                "You are a content writer. Based on the research provided, write clear, \
                engaging content. Structure it with headings and paragraphs.",
            )
            .build()?,
    );

    let coder_agent = Arc::new(
        LlmAgentBuilder::new("coder")
            .description("Code specialist")
            .model(model.clone())
            .instruction(
                "You are a coding specialist. Write clean, well-documented code examples \
                related to the topic. Include comments explaining the code.",
            )
            .build()?,
    );

    // Create AgentNodes with appropriate mappers
    let supervisor_node = AgentNode::new(supervisor_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let history =
                state.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

            let history_str: Vec<String> = history
                .iter()
                .filter_map(|h| {
                    let agent = h.get("agent").and_then(|a| a.as_str())?;
                    Some(format!("- {} completed their work", agent))
                })
                .collect();

            let prompt = format!(
                "Task: {}\n\nWork completed:\n{}\n\nWho should work next?",
                task,
                if history_str.is_empty() {
                    "None yet".to_string()
                } else {
                    history_str.join("\n")
                }
            );

            adk_core::Content::new("user").with_text(&prompt)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content
                        .parts
                        .iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("")
                        .to_lowercase();

                    let next = if text.contains("researcher") {
                        "researcher"
                    } else if text.contains("writer") {
                        "writer"
                    } else if text.contains("coder") {
                        "coder"
                    } else {
                        "done"
                    };

                    updates.insert("next_agent".to_string(), json!(next));
                    updates.insert("supervisor_reasoning".to_string(), json!(text.trim()));
                }
            }
            updates
        });

    let researcher_node = AgentNode::new(researcher_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Research this topic: {}", task))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("research_output".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let writer_node = AgentNode::new(writer_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let research = state.get("research_output").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user")
                .with_text(format!("Write content about: {}\n\nResearch:\n{}", task, research))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("written_content".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    let coder_node = AgentNode::new(coder_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Write code for: {}", task))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("code_output".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build the supervisor graph
    let graph = StateGraph::with_channels(&[
        "task",
        "next_agent",
        "history",
        "supervisor_reasoning",
        "research_output",
        "written_content",
        "code_output",
        "final_result",
    ])
    .add_node(supervisor_node)
    .add_node(researcher_node)
    .add_node(writer_node)
    .add_node(coder_node)
    // History tracking nodes
    .add_node_fn("track_researcher", |ctx| async move {
        let mut history =
            ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        history.push(json!({"agent": "researcher"}));
        println!("[researcher] Research completed");
        Ok(NodeOutput::new().with_update("history", json!(history)))
    })
    .add_node_fn("track_writer", |ctx| async move {
        let mut history =
            ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        history.push(json!({"agent": "writer"}));
        println!("[writer] Writing completed");
        Ok(NodeOutput::new().with_update("history", json!(history)))
    })
    .add_node_fn("track_coder", |ctx| async move {
        let mut history =
            ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        history.push(json!({"agent": "coder"}));
        println!("[coder] Coding completed");
        Ok(NodeOutput::new().with_update("history", json!(history)))
    })
    // Finalize node
    .add_node_fn("finalize", |ctx| async move {
        let history = ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let research = ctx.get("research_output").and_then(|v| v.as_str()).unwrap_or("");
        let content = ctx.get("written_content").and_then(|v| v.as_str()).unwrap_or("");
        let code = ctx.get("code_output").and_then(|v| v.as_str()).unwrap_or("");

        println!("\n[finalize] Compiling results from {} agents", history.len());

        let result = format!(
            "=== FINAL DELIVERABLE ===\n\n\
                Agents involved: {}\n\n\
                --- RESEARCH ---\n{}\n\n\
                --- CONTENT ---\n{}\n\n\
                --- CODE ---\n{}",
            history.len(),
            if research.is_empty() { "N/A" } else { research },
            if content.is_empty() { "N/A" } else { content },
            if code.is_empty() { "N/A" } else { code }
        );

        Ok(NodeOutput::new().with_update("final_result", json!(result)))
    })
    // Graph edges
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
    .with_recursion_limit(20);

    // Test tasks
    let tasks = ["Create a brief guide about Rust error handling with examples"];

    for task in &tasks {
        println!("{}", "=".repeat(70));
        println!("TASK: \"{}\"\n", task);

        let mut input = State::new();
        input.insert("task".to_string(), json!(task));
        input.insert("history".to_string(), json!([]));

        let result = graph.invoke(input, ExecutionConfig::new("supervisor-thread")).await?;

        println!("\n{}", "=".repeat(70));
        println!("{}", result.get("final_result").and_then(|v| v.as_str()).unwrap_or("No result"));
    }

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - Supervisor pattern with AgentNode wrapping multiple LlmAgents");
    println!("  - Dynamic routing based on LLM decisions");
    println!("  - Cyclic workflow with work tracking");
    println!("  - Multi-agent coordination");
    Ok(())
}
