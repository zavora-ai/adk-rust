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
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    println!("👥 Starting Supervisor Routing Example");
    println!("This demonstrates routing tasks to specialist agents\n");

    // Create supervisor agent
    let supervisor = Arc::new(
        LlmAgentBuilder::new("supervisor")
            .model(model.clone())
            .instruction(
                "You are a task supervisor. Route tasks to the appropriate specialist:\n\
                - 'researcher' for research, analysis, or information gathering tasks\n\
                - 'writer' for writing, editing, or content creation tasks\n\
                - 'coder' for programming, technical, or development tasks\n\
                - 'done' if the task is complete or no specialist is needed\n\n\
                Reply with ONLY the specialist name: researcher, writer, coder, or done."
            )
            .build()?,
    );

    // Create specialist agents
    let researcher = Arc::new(
        LlmAgentBuilder::new("researcher")
            .model(model.clone())
            .instruction(
                "You are a research specialist. Analyze the task and provide detailed research findings. \
                Be thorough and cite sources when possible. Keep responses under 3 sentences."
            )
            .build()?,
    );

    let writer = Arc::new(
        LlmAgentBuilder::new("writer")
            .model(model.clone())
            .instruction(
                "You are a writing specialist. Create clear, engaging content based on the task. \
                Focus on structure, clarity, and audience engagement. Keep responses under 3 sentences."
            )
            .build()?,
    );

    let coder = Arc::new(
        LlmAgentBuilder::new("coder")
            .model(model.clone())
            .instruction(
                "You are a coding specialist. Provide technical solutions, code examples, or development guidance. \
                Be precise and include best practices. Keep responses under 3 sentences."
            )
            .build()?,
    );

    // Create supervisor node
    let supervisor_node = AgentNode::new(supervisor)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            let history = state.get("work_done").and_then(|v| v.as_str()).unwrap_or("");
            let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
            
            let prompt = if history.is_empty() {
                format!("Task: {}", task)
            } else {
                format!("Task: {}\n\nWork completed so far:\n{}\n\nIteration: {}\n\nIs this task complete? If yes, respond 'done'. If more work needed, specify: researcher, writer, or coder.", task, history, iteration)
            };
            
            println!("🎯 Supervisor analyzing (iteration {}): {}", iteration, task);
            adk_core::Content::new("user").with_text(&prompt)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("")
                        .to_lowercase()
                        .trim()
                        .to_string();

                    let next = if text.contains("done") || text.contains("complete") { "done" }
                        else if text.contains("researcher") { "researcher" }
                        else if text.contains("writer") { "writer" }
                        else if text.contains("coder") { "coder" }
                        else { "done" }; // Default to done if unclear

                    println!("📋 Supervisor decision: Route to {}", next);
                    updates.insert("next_agent".to_string(), json!(next));
                }
            }
            updates
        });

    // Create specialist nodes
    let researcher_node = AgentNode::new(researcher)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            println!("🔍 Researcher working on: {}", task);
            adk_core::Content::new("user").with_text(task)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    
                    let work_done = format!("Research completed: {}", text);
                    println!("📊 Research result: {}", text);
                    updates.insert("work_done".to_string(), json!(work_done));
                }
            }
            updates
        });

    let writer_node = AgentNode::new(writer)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            println!("✍️  Writer working on: {}", task);
            adk_core::Content::new("user").with_text(task)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    
                    let work_done = format!("Writing completed: {}", text);
                    println!("📝 Writing result: {}", text);
                    updates.insert("work_done".to_string(), json!(work_done));
                }
            }
            updates
        });

    let coder_node = AgentNode::new(coder)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            println!("💻 Coder working on: {}", task);
            adk_core::Content::new("user").with_text(task)
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    
                    let work_done = format!("Coding completed: {}", text);
                    println!("⚡ Coding result: {}", text);
                    updates.insert("work_done".to_string(), json!(work_done));
                }
            }
            updates
        });

    // Build supervisor graph
    let graph = StateGraph::with_channels(&["task", "next_agent", "work_done", "iteration"])
        .add_node(supervisor_node)
        .add_node(researcher_node)
        .add_node(writer_node)
        .add_node(coder_node)
        .add_node_fn("counter", |ctx| async move {
            let i = ctx.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
            Ok(NodeOutput::new().with_update("iteration", json!(i + 1)))
        })
        .add_edge(START, "counter")
        .add_edge("counter", "supervisor")
        .add_conditional_edges(
            "supervisor",
            |state| {
                let next = state.get("next_agent").and_then(|v| v.as_str()).unwrap_or("done");
                let iteration = state.get("iteration").and_then(|v| v.as_i64()).unwrap_or(0);
                
                // Safety limit - max 3 iterations
                if iteration >= 3 {
                    println!("⚠️  Maximum iterations reached - completing task");
                    return END.to_string();
                }
                
                next.to_string()
            },
            [
                ("researcher", "researcher"),
                ("writer", "writer"),
                ("coder", "coder"),
                ("done", END),
                (END, END),
            ],
        )
        // Agents report back to supervisor
        .add_edge("researcher", "counter")
        .add_edge("writer", "counter")
        .add_edge("coder", "counter")
        .compile()?
        .with_recursion_limit(10);

    // Test with different task types
    let test_tasks = vec![
        "Research the benefits of renewable energy",
        "Write a blog post about AI in healthcare",
        "Create a Python function to calculate fibonacci numbers",
    ];

    for (i, task) in test_tasks.iter().enumerate() {
        println!("\n🔄 Task {}: {}", i + 1, task);
        
        let mut input = State::new();
        input.insert("task".to_string(), json!(task));

        let result = graph.invoke(input, ExecutionConfig::new(&format!("task-{}", i + 1))).await?;
        
        println!("✅ Final result: {}", result.get("work_done").and_then(|v| v.as_str()).unwrap_or("No work completed"));
        println!("{}", "─".repeat(60));
    }

    println!("\n🎉 Supervisor routing demonstration complete!");

    Ok(())
}
