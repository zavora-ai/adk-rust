//! Human-in-the-Loop (HITL) with LLM Planning
//!
//! This example demonstrates interrupt-based human intervention in graph workflows
//! using AgentNode for LLM-based planning that requires human approval.
//!
//! Graph: START -> planner -> review -> [interrupt if risky] -> executor -> END
//!
//! Run with: cargo run --example graph_hitl
//!
//! Requires: GOOGLE_API_KEY or GEMINI_API_KEY environment variable

use adk_agent::LlmAgentBuilder;
use adk_graph::{
    checkpoint::MemoryCheckpointer,
    edge::{END, START},
    error::GraphError,
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig, NodeOutput},
    state::State,
};
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Human-in-the-Loop with LLM Planning ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
    {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY or GEMINI_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example graph_hitl");
            return Ok(());
        }
    };

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create a checkpointer to persist state across interrupts
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // Create LLM planner agent
    let planner_agent = Arc::new(
        LlmAgentBuilder::new("planner")
            .description("Creates execution plans")
            .model(model.clone())
            .instruction(
                "You are a task planner. Analyze the given task and create a detailed execution plan. \
                Also assess the risk level (low/medium/high) based on potential impact. \
                Format: Start with 'RISK: [level]' on the first line, then list the steps.",
            )
            .build()?,
    );

    // Create LLM executor agent
    let executor_agent = Arc::new(
        LlmAgentBuilder::new("executor")
            .description("Executes approved plans")
            .model(model.clone())
            .instruction(
                "You are a task executor. Execute the given plan and provide a detailed report \
                of what was done and the results. Be thorough but concise.",
            )
            .build()?,
    );

    // Create AgentNodes
    let planner_node = AgentNode::new(planner_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user").with_text(format!("Create a plan for: {}", task))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");

                    if !text.is_empty() {
                        // Extract risk level from response
                        let risk_level = if text.to_lowercase().contains("risk: high") {
                            "high"
                        } else if text.to_lowercase().contains("risk: medium") {
                            "medium"
                        } else {
                            "low"
                        };

                        println!("[planner] Risk level: {}", risk_level);
                        updates.insert("plan".to_string(), json!(text));
                        updates.insert("risk_level".to_string(), json!(risk_level));
                    }
                }
            }
            updates
        });

    let executor_node = AgentNode::new(executor_agent)
        .with_input_mapper(|state| {
            let plan = state.get("plan").and_then(|v| v.as_str()).unwrap_or("");
            adk_core::Content::new("user")
                .with_text(format!("Execute this approved plan:\n{}", plan))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String =
                        content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");
                    if !text.is_empty() {
                        updates.insert("result".to_string(), json!(text));
                    }
                }
            }
            updates
        });

    // Build a workflow that requires human approval for risky tasks
    let graph = StateGraph::with_channels(&["task", "plan", "risk_level", "approved", "result"])
        .add_node(planner_node)
        .add_node(executor_node)
        // Review node: checks risk and may interrupt
        .add_node_fn("review", |ctx| async move {
            let risk_level = ctx.get("risk_level").and_then(|v| v.as_str()).unwrap_or("low");
            let plan = ctx.get("plan").and_then(|v| v.as_str()).unwrap_or("No plan");
            let approved = ctx.get("approved").and_then(|v| v.as_bool());

            println!("[review] Checking approval for {} risk task...", risk_level);

            // If already approved, continue
            if approved == Some(true) {
                println!("[review] Already approved, proceeding to execution...");
                return Ok(NodeOutput::new());
            }

            // For high/medium risk, require human approval
            if risk_level == "high" || risk_level == "medium" {
                println!("[review] {} risk - requesting human approval", risk_level.to_uppercase());
                return Ok(NodeOutput::interrupt_with_data(
                    &format!("{} RISK: Human approval required", risk_level.to_uppercase()),
                    json!({
                        "plan": plan,
                        "risk_level": risk_level,
                        "action": "Set 'approved' to true to continue"
                    }),
                ));
            }

            // Low risk: auto-approve
            println!("[review] Low risk - auto-approving");
            Ok(NodeOutput::new().with_update("approved", json!(true)))
        })
        .add_edge(START, "planner")
        .add_edge("planner", "review")
        .add_edge("review", "executor")
        .add_edge("executor", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());

    // ========== Test 1: Low Risk Task (auto-approved) ==========
    println!("{}", "=".repeat(60));
    println!("TEST 1: Low Risk Task (auto-approved)");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Read and summarize the README.md file"));

    let result = graph.invoke(input, ExecutionConfig::new("low-risk-001")).await?;

    println!("\nResult:\n{}", result.get("result").and_then(|v| v.as_str()).unwrap_or("None"));

    // ========== Test 2: High Risk Task (requires approval) ==========
    println!("\n{}", "=".repeat(60));
    println!("TEST 2: High Risk Task (requires human approval)");
    println!("{}", "=".repeat(60));

    let mut input = State::new();
    input.insert("task".to_string(), json!("Delete all backup files from the production server"));

    let thread_id = "high-risk-001";
    let result = graph.invoke(input, ExecutionConfig::new(thread_id)).await;

    match result {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("\n*** EXECUTION PAUSED ***");
            println!("Interrupt: {}", interrupted.interrupt);
            println!("Thread: {}", interrupted.thread_id);
            println!("\nPlan awaiting approval:");
            println!("{}", interrupted.state.get("plan").and_then(|v| v.as_str()).unwrap_or("?"));

            // Simulate human review
            println!("\n[HUMAN REVIEWER]");
            println!("Reviewing plan... This is a dangerous operation.");
            println!("After careful consideration, approving with caution.");

            // Update state with approval
            graph.update_state(thread_id, [("approved".to_string(), json!(true))]).await?;

            // Resume execution
            println!("\n*** RESUMING EXECUTION ***\n");
            let final_result = graph.invoke(State::new(), ExecutionConfig::new(thread_id)).await?;

            println!(
                "\nFinal Result:\n{}",
                final_result.get("result").and_then(|v| v.as_str()).unwrap_or("None")
            );
        }
        Ok(state) => {
            println!("Completed without interrupt: {:?}", state.get("result"));
        }
        Err(e) => {
            println!("Error: {}", e);
        }
    }

    // ========== Test 3: Static interrupt_before ==========
    println!("\n{}", "=".repeat(60));
    println!("TEST 3: Static interrupt_before node");
    println!("{}", "=".repeat(60));

    // Create a graph with static interrupt_before executor
    let graph_static = StateGraph::with_channels(&["task", "plan", "result"])
        .add_node(
            AgentNode::new(Arc::new(
                LlmAgentBuilder::new("simple_planner")
                    .model(model.clone())
                    .instruction("Create a brief 2-step plan for the task.")
                    .build()?,
            ))
            .with_input_mapper(|state| {
                let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
                adk_core::Content::new("user").with_text(task)
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
                            .join("");
                        if !text.is_empty() {
                            updates.insert("plan".to_string(), json!(text));
                        }
                    }
                }
                updates
            }),
        )
        .add_node_fn("execute", |ctx| async move {
            let plan = ctx.get("plan").and_then(|v| v.as_str()).unwrap_or("");
            println!("[execute] Running plan: {}", &plan[..plan.len().min(50)]);
            Ok(NodeOutput::new().with_update("result", json!("Execution complete")))
        })
        .add_edge(START, "simple_planner")
        .add_edge("simple_planner", "execute")
        .add_edge("execute", END)
        .compile()?
        .with_interrupt_before(&["execute"]); // Always pause before execute

    let mut input = State::new();
    input.insert("task".to_string(), json!("Organize project files"));

    let result = graph_static.invoke(input, ExecutionConfig::new("static-interrupt")).await;

    match result {
        Err(GraphError::Interrupted(interrupted)) => {
            println!("\n*** PAUSED BEFORE EXECUTE (static interrupt) ***");
            println!(
                "Plan created:\n{}",
                interrupted.state.get("plan").and_then(|v| v.as_str()).unwrap_or("?")
            );
            println!("\n(Would resume with graph.invoke() after human review)");
        }
        Ok(_) => println!("Completed unexpectedly"),
        Err(e) => println!("Error: {}", e),
    }

    println!("\n=== Complete ===");
    println!("\nThis example demonstrated:");
    println!("  - LLM-based planning with AgentNode");
    println!("  - Dynamic interrupts based on risk assessment");
    println!("  - Human approval workflow");
    println!("  - Static interrupt_before for mandatory review points");
    println!("  - Resume from checkpoint after approval");
    Ok(())
}
