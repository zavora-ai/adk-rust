//! Customer Service Multi-Agent System
//!
//! Demonstrates a coordinator agent that routes customer queries to specialists:
//! - Billing Agent: Handles payment, invoice, subscription questions
//! - Support Agent: Handles technical issues and troubleshooting
//!
//! Run with: cargo run --bin customer_service

use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    // Specialist: Billing Agent
    let billing_agent = LlmAgentBuilder::new("billing_agent")
        .description("Handles billing questions: payments, invoices, subscriptions, refunds")
        .instruction("You are a billing specialist. Help customers with:\n\
                     - Invoice questions and payment history\n\
                     - Subscription plans and upgrades\n\
                     - Refund requests\n\
                     - Payment method updates\n\
                     Be professional and provide clear information about billing matters.")
        .model(model.clone())
        .build()?;

    // Specialist: Technical Support Agent
    let support_agent = LlmAgentBuilder::new("support_agent")
        .description("Handles technical support: bugs, errors, troubleshooting, how-to questions")
        .instruction("You are a technical support specialist. Help customers with:\n\
                     - Troubleshooting errors and bugs\n\
                     - How-to questions about using the product\n\
                     - Configuration and setup issues\n\
                     - Performance problems\n\
                     Be patient and provide step-by-step guidance.")
        .model(model.clone())
        .build()?;

    // Coordinator: Routes to appropriate specialist
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Main customer service coordinator")
        .instruction("You are a customer service coordinator. Analyze each customer request:\n\n\
                     - For BILLING questions (payments, invoices, subscriptions, refunds):\n\
                       Transfer to billing_agent\n\n\
                     - For TECHNICAL questions (errors, bugs, how-to, troubleshooting):\n\
                       Transfer to support_agent\n\n\
                     - For GENERAL greetings or unclear requests:\n\
                       Respond yourself and ask clarifying questions\n\n\
                     When transferring, briefly acknowledge the customer and explain the handoff.")
        .model(model.clone())
        .sub_agent(Arc::new(billing_agent))
        .sub_agent(Arc::new(support_agent))
        .build()?;

    println!("🏢 Customer Service Center");
    println!("   Coordinator → Billing Agent | Support Agent");
    println!();
    println!("Example prompts:");
    println!("   • 'I have a question about my last invoice'");
    println!("   • 'The app keeps crashing when I open it'");
    println!("   • 'How do I upgrade my subscription?'");
    println!("   • 'I'm getting an error message'");
    println!();

    Launcher::new(Arc::new(coordinator)).run().await?;
    Ok(())
}
