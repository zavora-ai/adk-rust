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
use std::io;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    println!("🛡️  Starting Human-in-the-Loop Example");
    println!("This demonstrates risk-based approval workflow with dynamic interrupts\n");

    // Create planner agent that assesses risk
    let planner_agent = Arc::new(
        LlmAgentBuilder::new("planner")
            .description("Plans tasks and assesses risk")
            .model(model.clone())
            .instruction(
                "You are a task planner. Create a detailed plan for the given task and assess the risk level.\n\
                Always end your response with 'Risk: [HIGH/MEDIUM/LOW]' based on:\n\
                - HIGH: Involves sensitive data, financial transactions, or system changes\n\
                - MEDIUM: Involves user-facing changes or moderate complexity\n\
                - LOW: Simple, safe operations with minimal impact"
            )
            .build()?,
    );

    // Create executor agent
    let executor_agent = Arc::new(
        LlmAgentBuilder::new("executor")
            .description("Executes approved plans")
            .model(model.clone())
            .instruction(
                "You are a task executor. Execute the approved plan step by step. \
                Be thorough and report what you've accomplished."
            )
            .build()?,
    );

    // Create planner node with risk assessment
    let planner_node = AgentNode::new(planner_agent)
        .with_input_mapper(|state| {
            let task = state.get("task").and_then(|v| v.as_str()).unwrap_or("");
            println!("📋 Planning task: {}", task);
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

                    // Extract risk level from LLM response
                    let risk = if text.to_lowercase().contains("risk: high") { "high" }
                        else if text.to_lowercase().contains("risk: medium") { "medium" }
                        else { "low" };

                    println!("⚠️  Risk assessment: {}", risk.to_uppercase());
                    updates.insert("plan".to_string(), json!(text));
                    updates.insert("risk_level".to_string(), json!(risk));
                }
            }
            updates
        });

    // Create executor node
    let executor_node = AgentNode::new(executor_agent)
        .with_input_mapper(|state| {
            let plan = state.get("plan").and_then(|v| v.as_str()).unwrap_or("");
            println!("⚡ Executing approved plan...");
            adk_core::Content::new("user").with_text(&format!("Execute this plan: {}", plan))
        })
        .with_output_mapper(|events| {
            let mut updates = std::collections::HashMap::new();
            for event in events {
                if let Some(content) = event.content() {
                    let text: String = content.parts.iter()
                        .filter_map(|p| p.text())
                        .collect::<Vec<_>>()
                        .join("");
                    updates.insert("result".to_string(), json!(text));
                }
            }
            updates
        });

    // Create checkpointer for state persistence
    let checkpointer = Arc::new(MemoryCheckpointer::new());

    // Review node with dynamic interrupt
    let graph = StateGraph::with_channels(&["task", "plan", "risk_level", "approved", "result"])
        .add_node(planner_node)
        .add_node(executor_node)
        .add_node_fn("review", |ctx| async move {
            let risk = ctx.get("risk_level").and_then(|v| v.as_str()).unwrap_or("low");
            let approved = ctx.get("approved").and_then(|v| v.as_bool());

            // Already approved - continue
            if approved == Some(true) {
                println!("✅ Plan approved - proceeding to execution");
                return Ok(NodeOutput::new());
            }

            // High/medium risk - interrupt for approval
            if risk == "high" || risk == "medium" {
                println!("🛑 {} RISK DETECTED - Human approval required", risk.to_uppercase());
                return Ok(NodeOutput::interrupt_with_data(
                    &format!("{} RISK: Human approval required", risk.to_uppercase()),
                    json!({
                        "plan": ctx.get("plan"),
                        "risk_level": risk,
                        "action": "Set 'approved' to true to continue, or false to reject"
                    })
                ));
            }

            // Low risk - auto-approve
            println!("✅ Low risk - auto-approved");
            Ok(NodeOutput::new().with_update("approved", json!(true)))
        })
        .add_edge(START, "planner")
        .add_edge("planner", "review")
        .add_edge("review", "executor")
        .add_edge("executor", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone())
        .with_recursion_limit(3);

    // Test with different risk levels
    let test_tasks = vec![
        ("Update user documentation", "LOW"),
        ("Modify user interface layout", "MEDIUM"),
        ("Delete all user accounts", "HIGH"),
    ];

    for (task, expected_risk) in test_tasks {
        println!("\n🔄 Testing task: {}", task);
        println!("Expected risk level: {}", expected_risk);
        
        let mut input = State::new();
        input.insert("task".to_string(), json!(task));
        
        let thread_id = format!("task-{}", task.replace(" ", "-"));
        let result = graph.invoke(input, ExecutionConfig::new(&thread_id)).await;

        match result {
            Err(GraphError::Interrupted(interrupt)) => {
                println!("🛑 EXECUTION PAUSED");
                println!("Reason: {}", interrupt.interrupt);
                println!("Plan: {}", interrupt.state.get("plan").and_then(|v| v.as_str()).unwrap_or("N/A"));
                
                // Get real human input
                println!("\n👤 Human approval required!");
                println!("Do you approve this plan? (y/n): ");
                
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).expect("Failed to read input");
                let should_approve = input.trim().to_lowercase().starts_with('y');
                
                println!("👤 Human decision: {}", if should_approve { "APPROVED" } else { "REJECTED" });
                
                if should_approve {
                    // Update state with approval
                    graph.update_state(&thread_id, [("approved".to_string(), json!(true))]).await?;
                    
                    // Resume execution
                    let final_result = graph.invoke(State::new(), ExecutionConfig::new(&thread_id)).await?;
                    println!("✅ Task completed: {}", final_result.get("result").and_then(|v| v.as_str()).unwrap_or("N/A"));
                } else {
                    println!("❌ Task rejected - execution stopped");
                }
            }
            Ok(result) => {
                println!("✅ Task auto-approved and completed: {}", result.get("result").and_then(|v| v.as_str()).unwrap_or("N/A"));
            }
            Err(e) => {
                println!("❌ Error: {}", e);
            }
        }
        
        println!("{}", "─".repeat(60));
    }

    println!("\n🎉 Human-in-the-loop demonstration complete!");

    Ok(())
}
