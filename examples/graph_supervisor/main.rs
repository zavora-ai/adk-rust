//! Multi-Agent Supervisor Pattern
//!
//! This example demonstrates a supervisor agent that dynamically routes tasks
//! to specialized worker agents based on the task requirements.
//!
//! Graph:
//!   START -> supervisor -> [research | writer | coder | done] -> supervisor (cycle)
//!
//! Key concepts demonstrated:
//! - Multi-agent coordination
//! - Dynamic routing based on LLM decisions
//! - Agent specialization
//! - Cyclic workflows with termination conditions

use adk_graph::{
    edge::{END, START},
    graph::StateGraph,
    node::{ExecutionConfig, NodeOutput},
    state::State,
};
use serde_json::json;

/// Simulate supervisor deciding which agent to call next
fn supervisor_decide(task: &str, history: &[serde_json::Value]) -> (String, String) {
    // Analyze what's been done
    let completed_agents: Vec<&str> =
        history.iter().filter_map(|h| h.get("agent").and_then(|a| a.as_str())).collect();

    let task_lower = task.to_lowercase();

    // Decision logic based on task and history
    if completed_agents.is_empty() {
        // First step: analyze the task
        if task_lower.contains("research")
            || task_lower.contains("find")
            || task_lower.contains("learn")
        {
            ("research".to_string(), "Starting with research to gather information.".to_string())
        } else if task_lower.contains("write")
            || task_lower.contains("article")
            || task_lower.contains("blog")
        {
            ("research".to_string(), "Need to research before writing.".to_string())
        } else if task_lower.contains("code")
            || task_lower.contains("implement")
            || task_lower.contains("build")
        {
            ("coder".to_string(), "This is a coding task, delegating to coder.".to_string())
        } else {
            ("research".to_string(), "Starting with research to understand the task.".to_string())
        }
    } else if completed_agents.contains(&"research") && !completed_agents.contains(&"writer") {
        if task_lower.contains("write") || task_lower.contains("article") {
            ("writer".to_string(), "Research complete, now writing content.".to_string())
        } else if task_lower.contains("code") {
            ("coder".to_string(), "Research complete, now implementing code.".to_string())
        } else {
            ("done".to_string(), "Research complete, task finished.".to_string())
        }
    } else if completed_agents.contains(&"writer") && !completed_agents.contains(&"coder") {
        if task_lower.contains("code") {
            ("coder".to_string(), "Writing complete, adding code examples.".to_string())
        } else {
            ("done".to_string(), "Writing complete, task finished.".to_string())
        }
    } else if completed_agents.contains(&"coder") {
        ("done".to_string(), "All work complete!".to_string())
    } else {
        ("done".to_string(), "Task complete.".to_string())
    }
}

/// Simulate research agent
fn research_agent(topic: &str) -> serde_json::Value {
    json!({
        "agent": "research",
        "findings": format!(
            "Research on '{}': Found 5 key insights about the topic. \
             The main points are: 1) Historical context, 2) Current state, \
             3) Best practices, 4) Common pitfalls, 5) Future trends.",
            topic
        ),
        "sources": ["Wikipedia", "Technical docs", "Academic papers"]
    })
}

/// Simulate writer agent
fn writer_agent(topic: &str, research: &str) -> serde_json::Value {
    json!({
        "agent": "writer",
        "content": format!(
            "# Article: {}\n\n\
             Based on our research, here's a comprehensive overview...\n\n\
             ## Key Findings\n{}\n\n\
             ## Conclusion\n\
             In summary, {} represents an important topic that deserves attention.",
            topic,
            research,
            topic
        ),
        "word_count": 250
    })
}

/// Simulate coder agent
fn coder_agent(topic: &str) -> serde_json::Value {
    json!({
        "agent": "coder",
        "code": format!(
            "// Implementation for: {}\n\
             fn main() {{\n    \
                 println!(\"Hello, {}!\");\n    \
                 // TODO: Add more implementation\n\
             }}",
            topic, topic
        ),
        "language": "Rust",
        "tests_passing": true
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Multi-Agent Supervisor Pattern ===\n");

    // Build the supervisor graph
    let graph =
        StateGraph::with_channels(&["task", "next_agent", "history", "final_result", "reasoning"])
            // Supervisor: decides which agent to call
            .add_node_fn("supervisor", |ctx| async move {
                let task = ctx.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let history =
                    ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                println!("[supervisor] Analyzing task: \"{}\"", task);
                println!("[supervisor] Completed steps: {}", history.len());

                let (next_agent, reasoning) = supervisor_decide(task, &history);

                println!("[supervisor] Decision: {} ", next_agent);
                println!("[supervisor] Reasoning: {}", reasoning);

                Ok(NodeOutput::new()
                    .with_update("next_agent", json!(next_agent))
                    .with_update("reasoning", json!(reasoning)))
            })
            // Research agent
            .add_node_fn("research", |ctx| async move {
                let task = ctx.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let mut history =
                    ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                println!("\n[research] Conducting research on: {}", task);

                let result = research_agent(task);
                println!(
                    "[research] Found {} sources",
                    result.get("sources").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0)
                );

                history.push(result);

                Ok(NodeOutput::new().with_update("history", json!(history)))
            })
            // Writer agent
            .add_node_fn("writer", |ctx| async move {
                let task = ctx.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let mut history =
                    ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                // Get research findings
                let research_findings = history
                    .iter()
                    .find(|h| h.get("agent").and_then(|a| a.as_str()) == Some("research"))
                    .and_then(|h| h.get("findings"))
                    .and_then(|f| f.as_str())
                    .unwrap_or("No research available");

                println!("\n[writer] Writing content based on research...");

                let result = writer_agent(task, research_findings);
                println!(
                    "[writer] Produced {} words",
                    result.get("word_count").and_then(|w| w.as_i64()).unwrap_or(0)
                );

                history.push(result);

                Ok(NodeOutput::new().with_update("history", json!(history)))
            })
            // Coder agent
            .add_node_fn("coder", |ctx| async move {
                let task = ctx.get("task").and_then(|v| v.as_str()).unwrap_or("");
                let mut history =
                    ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                println!("\n[coder] Implementing code for: {}", task);

                let result = coder_agent(task);
                println!(
                    "[coder] Language: {}, Tests passing: {}",
                    result.get("language").and_then(|l| l.as_str()).unwrap_or("?"),
                    result.get("tests_passing").and_then(|t| t.as_bool()).unwrap_or(false)
                );

                history.push(result);

                Ok(NodeOutput::new().with_update("history", json!(history)))
            })
            // Finalize: compile results
            .add_node_fn("finalize", |ctx| async move {
                let history =
                    ctx.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();

                println!("\n[finalize] Compiling final results...");

                let agents_used: Vec<&str> = history
                    .iter()
                    .filter_map(|h| h.get("agent").and_then(|a| a.as_str()))
                    .collect();

                let summary = format!(
                    "Task completed using {} agent(s): {}",
                    agents_used.len(),
                    agents_used.join(" -> ")
                );

                println!("[finalize] {}", summary);

                Ok(NodeOutput::new().with_update(
                    "final_result",
                    json!({
                        "summary": summary,
                        "agents_used": agents_used,
                        "total_steps": history.len(),
                        "deliverables": history
                    }),
                ))
            })
            // Graph structure
            .add_edge(START, "supervisor")
            // Conditional routing from supervisor
            .add_conditional_edges(
                "supervisor",
                |state| {
                    state.get("next_agent").and_then(|v| v.as_str()).unwrap_or("done").to_string()
                },
                [
                    ("research", "research"),
                    ("writer", "writer"),
                    ("coder", "coder"),
                    ("done", "finalize"),
                ],
            )
            // All agents report back to supervisor
            .add_edge("research", "supervisor")
            .add_edge("writer", "supervisor")
            .add_edge("coder", "supervisor")
            .add_edge("finalize", END)
            .compile()?
            .with_recursion_limit(20);

    // ========== Test 1: Research + Write task ==========
    println!("{}", "=".repeat(60));
    println!("TASK 1: \"Research and write an article about Rust async\"");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Research and write an article about Rust async"));
    input.insert("history".to_string(), json!([]));

    let result = graph.invoke(input, ExecutionConfig::new("task-1")).await?;

    let final_result = result.get("final_result").unwrap();
    println!("\n{}", "=".repeat(60));
    println!("FINAL RESULT:");
    println!("  Summary: {}", final_result.get("summary").and_then(|s| s.as_str()).unwrap_or("?"));
    println!(
        "  Total steps: {}",
        final_result.get("total_steps").and_then(|t| t.as_i64()).unwrap_or(0)
    );

    // ========== Test 2: Coding task ==========
    println!("\n{}", "=".repeat(60));
    println!("TASK 2: \"Implement a simple web server\"");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Implement a simple web server"));
    input.insert("history".to_string(), json!([]));

    let result = graph.invoke(input, ExecutionConfig::new("task-2")).await?;

    let final_result = result.get("final_result").unwrap();
    println!("\n{}", "=".repeat(60));
    println!("FINAL RESULT:");
    println!("  Summary: {}", final_result.get("summary").and_then(|s| s.as_str()).unwrap_or("?"));

    // ========== Test 3: Full pipeline ==========
    println!("\n{}", "=".repeat(60));
    println!("TASK 3: \"Research, write documentation, and code a CLI tool\"");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Research, write documentation, and code a CLI tool"));
    input.insert("history".to_string(), json!([]));

    let result = graph.invoke(input, ExecutionConfig::new("task-3")).await?;

    let final_result = result.get("final_result").unwrap();
    println!("\n{}", "=".repeat(60));
    println!("FINAL RESULT:");
    println!("  Summary: {}", final_result.get("summary").and_then(|s| s.as_str()).unwrap_or("?"));
    println!("  Agents: {:?}", final_result.get("agents_used"));

    println!("\n=== Complete ===");
    Ok(())
}
