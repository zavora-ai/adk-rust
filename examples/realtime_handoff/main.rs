//! Realtime Multi-Agent Handoff Example.
//!
//! This example demonstrates how to use RealtimeAgent with sub-agents
//! for transferring conversations between specialized agents.
//!
//! Architecture:
//! ```
//!                ┌─────────────────┐
//!                │  Receptionist   │ (entry point)
//!                │  RealtimeAgent  │
//!                └────────┬────────┘
//!                         │
//!        ┌────────────────┼────────────────┐
//!        ▼                ▼                ▼
//! ┌─────────────┐  ┌─────────────┐  ┌─────────────┐
//! │   Booking   │  │   Support   │  │    Sales    │
//! │    Agent    │  │    Agent    │  │    Agent    │
//! └─────────────┘  └─────────────┘  └─────────────┘
//! ```
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example realtime_handoff --features realtime-openai
//! ```

use adk_core::Agent; // Import the Agent trait for name(), description(), sub_agents()
use adk_realtime::{
    RealtimeAgent, RealtimeConfig, RealtimeModel, ServerEvent, ToolResponse,
    config::ToolDefinition, openai::OpenAIRealtimeModel,
};
use serde_json::json;
use std::sync::Arc;

/// Create a simulated booking agent
fn create_booking_agent(model: Arc<dyn RealtimeModel>) -> RealtimeAgent {
    RealtimeAgent::builder("booking_agent")
        .model(model)
        .description("Handles reservations and booking requests")
        .instruction(
            "You are a booking specialist. Help customers with:\
             - Restaurant reservations\
             - Hotel bookings\
             - Event tickets\
             - Travel arrangements\
             Be efficient and confirm all booking details.",
        )
        .voice("coral")
        .build()
        .expect("Failed to build booking agent")
}

/// Create a simulated support agent
fn create_support_agent(model: Arc<dyn RealtimeModel>) -> RealtimeAgent {
    RealtimeAgent::builder("support_agent")
        .model(model)
        .description("Handles technical support and troubleshooting")
        .instruction(
            "You are a technical support specialist. Help customers with:\
             - Account issues\
             - Technical problems\
             - Bug reports\
             - Feature questions\
             Be patient and thorough in your troubleshooting.",
        )
        .voice("sage")
        .build()
        .expect("Failed to build support agent")
}

/// Create a simulated sales agent
fn create_sales_agent(model: Arc<dyn RealtimeModel>) -> RealtimeAgent {
    RealtimeAgent::builder("sales_agent")
        .model(model)
        .description("Handles product inquiries and sales")
        .instruction(
            "You are a friendly sales representative. Help customers with:\
             - Product information\
             - Pricing questions\
             - Promotions and discounts\
             - Purchase assistance\
             Be helpful but not pushy.",
        )
        .voice("shimmer")
        .build()
        .expect("Failed to build sales agent")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== ADK-Rust Realtime Multi-Agent Handoff Example ===\n");

    // Create the shared model as a trait object
    let model: Arc<dyn RealtimeModel> =
        Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    // Create sub-agents
    let booking_agent = Arc::new(create_booking_agent(Arc::clone(&model)));
    let support_agent = Arc::new(create_support_agent(Arc::clone(&model)));
    let sales_agent = Arc::new(create_sales_agent(Arc::clone(&model)));

    // Create the main receptionist agent with sub-agents
    let receptionist = RealtimeAgent::builder("receptionist")
        .model(Arc::clone(&model))
        .description("Main entry point - routes to specialized agents")
        .instruction(
            "You are a friendly receptionist named Alex. Greet customers warmly and help route them:\
             \
             - For reservations or bookings → transfer to booking_agent\
             - For technical issues or support → transfer to support_agent\
             - For product info or purchases → transfer to sales_agent\
             \
             Ask clarifying questions if needed, then transfer to the appropriate agent.\
             Use the transfer_to_agent tool when ready to hand off.",
        )
        .voice("alloy")
        .sub_agent(booking_agent)
        .sub_agent(support_agent)
        .sub_agent(sales_agent)
        .build()?;

    println!("Created agent hierarchy:");
    println!("  📞 {} - Receptionist (entry point)", receptionist.name());
    for sub_agent in receptionist.sub_agents() {
        println!("    └─ {} - {}", sub_agent.name(), sub_agent.description());
    }
    println!();

    // Build the transfer_to_agent tool with available agent names
    let agent_names: Vec<String> =
        receptionist.sub_agents().iter().map(|a| a.name().to_string()).collect();

    let transfer_tool = ToolDefinition {
        name: "transfer_to_agent".to_string(),
        description: Some(format!(
            "Transfer the conversation to a specialized agent. Available agents: {}",
            agent_names.join(", ")
        )),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "agent_name": {
                    "type": "string",
                    "enum": agent_names,
                    "description": "The name of the agent to transfer to"
                },
                "reason": {
                    "type": "string",
                    "description": "Brief reason for the transfer"
                }
            },
            "required": ["agent_name"]
        })),
    };

    // Get the instruction from the agent
    let instruction = receptionist.instruction().unwrap_or("You are a friendly receptionist.");

    // Connect the receptionist agent with the transfer tool
    let config = RealtimeConfig::default()
        .with_instruction(instruction)
        .with_tools(vec![transfer_tool])
        .with_modalities(vec!["text".to_string()]);

    println!("Connecting receptionist agent...");

    let session = model.connect(config).await?;

    println!("Connected!\n");

    // Simulate a customer conversation that requires handoff
    let scenarios = vec![
        ("Hi, I need help with a hotel booking for next week.", "booking_agent"),
        ("I'm having trouble logging into my account.", "support_agent"),
        ("Can you tell me about your premium plans?", "sales_agent"),
    ];

    for (customer_message, expected_handoff) in scenarios {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Customer: {}\n", customer_message);

        session.send_text(customer_message).await?;
        session.create_response().await?;

        print!("Receptionist: ");

        let mut handoff_detected = None;

        let mut awaiting_tool_response = false;

        while let Some(event_result) = session.next_event().await {
            match event_result {
                Ok(event) => match event {
                    ServerEvent::TextDelta { delta, .. } => {
                        print!("{}", delta);
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                    }
                    ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
                        if name == "transfer_to_agent"
                            && let Ok(args) = serde_json::from_str::<serde_json::Value>(&arguments)
                        {
                            let agent_name = args
                                .get("agent_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let reason = args
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("No reason provided");

                            handoff_detected = Some(agent_name.to_string());
                            println!("\n\n🔄 [Handoff to: {} - {}]", agent_name, reason);

                            // Send tool response to acknowledge the transfer
                            let response = ToolResponse::new(
                                &call_id,
                                json!({
                                    "status": "transferred",
                                    "agent": agent_name,
                                    "message": format!("Successfully transferred to {}", agent_name)
                                }),
                            );
                            session.send_tool_response(response).await?;
                            awaiting_tool_response = true;
                        }
                    }
                    ServerEvent::ResponseDone { .. } => {
                        if awaiting_tool_response {
                            // The model may respond after the tool response
                            awaiting_tool_response = false;
                            // Continue to get the final message
                        } else if handoff_detected.is_some() {
                            // Handoff complete, break out
                            println!();
                            break;
                        } else {
                            println!("\n");
                            break;
                        }
                    }
                    ServerEvent::Error { error, .. } => {
                        eprintln!("\nError: {}", error.message);
                        break;
                    }
                    _ => {}
                },
                Err(e) => {
                    eprintln!("Error: {}", e);
                    break;
                }
            }
        }

        // Verify expected handoff (for demonstration)
        if let Some(ref agent) = handoff_detected {
            println!("✅ Expected: {}, Got: {}", expected_handoff, agent);

            // In a real application, you would:
            // 1. Find the sub-agent by name
            // 2. Create a new session with that agent's configuration
            // 3. Transfer conversation context
            // 4. Continue the conversation with the new agent
        } else {
            println!("ℹ️  No handoff was triggered (assistant may be gathering more info)");
        }

        println!();
    }

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("=== Agent Handoff Patterns ===\n");
    println!("In a production system, agent handoffs work like this:");
    println!();
    println!("1. RealtimeAgent has sub_agents registered");
    println!("2. Each sub_agent automatically creates a transfer_to_agent tool");
    println!("3. When the LLM calls transfer_to_agent(agent_name=\"...\"):");
    println!("   - The runner detects the handoff request");
    println!("   - Conversation context is preserved");
    println!("   - A new session starts with the target agent");
    println!("   - The target agent continues the conversation");
    println!();
    println!("This enables complex multi-agent workflows like:");
    println!("  • Customer service routing");
    println!("  • Escalation paths");
    println!("  • Specialized task handling");
    println!("  • Multi-step workflows");

    println!("\n=== Session Complete ===");

    Ok(())
}
