//! Realtime Multi-Agent Handoff Example
//! 
//! Demonstrates transferring conversations between specialized agents.

use adk_realtime::{
    openai::OpenAIRealtimeModel,
    RealtimeAgent, RealtimeModel,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY")?;

    println!("🔄 Realtime Multi-Agent Handoff Example");
    println!("This demonstrates transferring conversations between agents\n");

    // Create the realtime model
    let model: Arc<dyn RealtimeModel> = Arc::new(
        OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17")
    );

    // Create specialized sub-agents
    let booking_agent = Arc::new(
        RealtimeAgent::builder("booking_agent")
            .model(model.clone())
            .instruction(
                "You are a booking specialist. Help customers make reservations. \
                 Be friendly and efficient. Ask for date, time, and party size."
            )
            .voice("coral")
            .build()?
    );

    let support_agent = Arc::new(
        RealtimeAgent::builder("support_agent")
            .model(model.clone())
            .instruction(
                "You are a technical support specialist. Help customers with issues. \
                 Be patient and thorough. Ask clarifying questions."
            )
            .voice("sage")
            .build()?
    );

    let billing_agent = Arc::new(
        RealtimeAgent::builder("billing_agent")
            .model(model.clone())
            .instruction(
                "You are a billing specialist. Help with payments and invoices. \
                 Be professional and accurate."
            )
            .voice("shimmer")
            .build()?
    );

    // Create main receptionist agent with sub-agents
    let _receptionist = RealtimeAgent::builder("receptionist")
        .model(model)
        .instruction(
            "You are a receptionist. Greet customers and route them to the right specialist:\n\
             - For reservations or bookings → transfer to booking_agent\n\
             - For technical issues or problems → transfer to support_agent\n\
             - For billing, payments, or invoices → transfer to billing_agent\n\n\
             Use the transfer_to_agent tool to hand off conversations."
        )
        .voice("alloy")
        .sub_agent(booking_agent)
        .sub_agent(support_agent)
        .sub_agent(billing_agent)
        .server_vad()
        .build()?;

    println!("📋 Agent Configuration:");
    println!("   Main: receptionist (voice: alloy)");
    println!("   Sub-agents:");
    println!("     - booking_agent (voice: coral)");
    println!("     - support_agent (voice: sage)");
    println!("     - billing_agent (voice: shimmer)\n");

    println!("🎯 The receptionist will automatically route customers to specialists");
    println!("   based on their needs using the transfer_to_agent tool.\n");

    // Display the agent structure
    println!("📊 Agent Hierarchy:");
    println!("   receptionist");
    println!("   ├── booking_agent");
    println!("   ├── support_agent");
    println!("   └── billing_agent\n");

    println!("✅ Multi-agent setup complete!");
    println!("\n📝 How handoffs work:");
    println!("   1. User talks to receptionist");
    println!("   2. Receptionist identifies the need");
    println!("   3. Receptionist calls transfer_to_agent(\"booking_agent\")");
    println!("   4. RealtimeRunner handles the handoff automatically");
    println!("   5. User now talks to booking_agent");

    // Note: Actually running the conversation would require the RealtimeRunner
    // which handles the event loop and handoffs. This example shows the setup.

    Ok(())
}
