//! Validates: docs/official_docs/agents/multi-agent.md
//!
//! This example demonstrates multi-agent systems with sub-agent hierarchies.
//! It shows how to create a coordinator agent that delegates to specialized
//! sub-agents based on the type of request.
//!
//! Run modes:
//!   cargo run --example multi_agent -p adk-rust-guide              # Validation mode
//!   cargo run --example multi_agent -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example multi_agent -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model (shared across agents)
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create specialized sub-agents
    
    // Billing agent - handles payment and invoice questions
    let billing_agent = LlmAgentBuilder::new("billing_agent")
        .description("Handles all billing, payment, and invoice questions")
        .instruction(
            "You are a billing specialist. Answer questions about:\n\
             - Payment methods and processing\n\
             - Invoice details and history\n\
             - Subscription plans and pricing\n\
             - Refunds and credits\n\
             Be clear, accurate, and helpful."
        )
        .model(model.clone())
        .build()?;

    // Support agent - handles technical issues
    let support_agent = LlmAgentBuilder::new("support_agent")
        .description("Provides technical support and troubleshooting assistance")
        .instruction(
            "You are a technical support specialist. Help users with:\n\
             - Login and account access issues\n\
             - Software bugs and errors\n\
             - Feature usage and configuration\n\
             - Performance problems\n\
             Provide step-by-step troubleshooting guidance."
        )
        .model(model.clone())
        .build()?;

    // Info agent - handles general information requests
    let info_agent = LlmAgentBuilder::new("info_agent")
        .description("Provides general information about products and services")
        .instruction(
            "You are an information specialist. Answer questions about:\n\
             - Product features and capabilities\n\
             - Company policies and procedures\n\
             - General inquiries and FAQs\n\
             - Service availability and hours\n\
             Be informative and friendly."
        )
        .model(model.clone())
        .build()?;

    // Coordinator agent - routes requests to appropriate sub-agents
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Customer service coordinator that routes requests to specialists")
        .global_instruction(
            "You are a customer service representative for TechCorp. \
             Always maintain a professional and friendly tone. \
             Our values are: customer satisfaction, quick resolution, and clear communication."
        )
        .instruction(
            "You are the main customer service coordinator. \
             Analyze each user request and delegate to the appropriate specialist:\n\n\
             - For billing, payment, invoice, or subscription questions → transfer to billing_agent\n\
             - For technical issues, bugs, errors, or troubleshooting → transfer to support_agent\n\
             - For general information, product questions, or policies → transfer to info_agent\n\n\
             If you're unsure which specialist to use, ask the user for clarification."
        )
        .model(model.clone())
        .sub_agent(Arc::new(billing_agent))
        .sub_agent(Arc::new(support_agent))
        .sub_agent(Arc::new(info_agent))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(coordinator)).run().await?;
    } else {
        // Validation mode - verify the multi-agent system was created correctly
        print_validating("agents/multi-agent.md");

        // Verify coordinator properties
        println!("Coordinator name: {}", coordinator.name());
        println!("Coordinator description: {}", coordinator.description());
        println!("Number of sub-agents: {}", coordinator.sub_agents().len());

        // Verify the coordinator has the correct sub-agents
        assert_eq!(coordinator.name(), "coordinator");
        assert_eq!(coordinator.sub_agents().len(), 3);

        // Verify sub-agent names
        let sub_agent_names: Vec<&str> = coordinator
            .sub_agents()
            .iter()
            .map(|a| a.name())
            .collect();
        
        println!("Sub-agents: {:?}", sub_agent_names);
        
        assert!(sub_agent_names.contains(&"billing_agent"));
        assert!(sub_agent_names.contains(&"support_agent"));
        assert!(sub_agent_names.contains(&"info_agent"));

        // Verify sub-agent descriptions are set
        for sub_agent in coordinator.sub_agents() {
            assert!(!sub_agent.description().is_empty(), 
                "Sub-agent {} should have a description", sub_agent.name());
        }

        print_success("multi_agent");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example multi_agent -p adk-rust-guide -- chat");
        println!("\nTry asking:");
        println!("  - 'I have a question about my last invoice'");
        println!("  - 'I can't log into my account'");
        println!("  - 'What features does your product have?'");
    }

    Ok(())
}
