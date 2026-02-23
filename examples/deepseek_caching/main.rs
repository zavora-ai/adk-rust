//! DeepSeek Context Caching (KV Cache) Example
//!
//! This example demonstrates DeepSeek's automatic context caching feature.
//! When you send multiple requests with the same prefix (like a system message
//! or document), DeepSeek caches the processed tokens and reuses them.
//!
//! Key benefits:
//! - Cache hit tokens: 0.1 yuan per million (10x cheaper!)
//! - Cache miss tokens: 1 yuan per million
//! - Automatic - no configuration needed
//! - Works best with long, repeated prefixes (64+ tokens)
//!
//! The response includes `prompt_cache_hit_tokens` and `prompt_cache_miss_tokens`
//! in the usage metadata.
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! cargo run --example deepseek_caching --features deepseek
//! ```

use adk_agent::LlmAgentBuilder;
use adk_core::Content;
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use std::sync::Arc;

// A long document that will be cached across requests
const DOCUMENT: &str = r#"
# Company Policy Document: Remote Work Guidelines

## 1. Introduction
This document outlines the comprehensive guidelines for remote work at Acme Corporation.
All employees engaged in remote work arrangements must adhere to these policies to ensure
productivity, security, and work-life balance.

## 2. Eligibility
Remote work arrangements are available to full-time employees who have:
- Completed their probationary period (minimum 90 days)
- Demonstrated consistent performance ratings of "Meets Expectations" or higher
- Received manager approval for remote work
- Signed the Remote Work Agreement

## 3. Equipment and Technology
### 3.1 Company-Provided Equipment
- Laptop computer with standard software suite
- External monitor (upon request)
- Keyboard and mouse
- Headset for video conferencing

### 3.2 Employee Responsibilities
- Maintain reliable high-speed internet connection (minimum 50 Mbps)
- Ensure a dedicated workspace free from distractions
- Keep all equipment in good working condition
- Report any technical issues within 24 hours

## 4. Working Hours
### 4.1 Core Hours
All remote employees must be available during core business hours:
- Monday through Friday: 10:00 AM - 3:00 PM (local time)
- Flexibility is allowed outside core hours with manager approval

### 4.2 Time Tracking
- Log hours using the company's time tracking system
- Submit weekly timesheets by Friday 5:00 PM
- Overtime requires prior written approval

## 5. Communication
### 5.1 Tools
- Email for formal communications
- Slack for instant messaging and quick questions
- Zoom for video meetings
- Confluence for documentation

### 5.2 Response Times
- Email: Within 4 business hours
- Slack: Within 1 hour during core hours
- Urgent matters: Immediate response expected

## 6. Security
### 6.1 Data Protection
- Use VPN for all work-related activities
- Never share passwords or access credentials
- Report security incidents immediately
- Complete annual security training

### 6.2 Physical Security
- Lock computer when away from desk
- Secure physical documents
- Use privacy screens in public spaces

## 7. Performance and Accountability
- Weekly check-ins with direct manager
- Monthly performance reviews
- Quarterly goal setting and assessment
- Annual comprehensive performance evaluation

## 8. Expenses
Eligible expenses for reimbursement:
- Internet service (up to $50/month)
- Office supplies (up to $100/quarter)
- Ergonomic equipment (one-time up to $500)

## 9. Health and Safety
- Take regular breaks (5-10 minutes every hour)
- Ensure ergonomic workspace setup
- Report any work-related injuries
- Access to Employee Assistance Program (EAP)

## 10. Termination of Remote Work
Remote work privileges may be revoked for:
- Performance issues
- Policy violations
- Business needs requiring on-site presence
- Security breaches
"#;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create DeepSeek client
    let model = DeepSeekClient::new(DeepSeekConfig::chat(api_key))?;

    // Build agent with the long document as context
    let instruction = format!(
        "You are a helpful HR assistant. You have access to the following company policy document. \
         Answer questions about the policy accurately and concisely.\n\n\
         POLICY DOCUMENT:\n{}\n\n\
         When answering, cite the relevant section from the document.",
        DOCUMENT
    );

    let agent = LlmAgentBuilder::new("hr_assistant")
        .model(Arc::new(model))
        .instruction(instruction)
        .build()?;

    // Create session service and runner
    let session_service = Arc::new(InMemorySessionService::new());
    let session = session_service
        .create(CreateRequest {
            app_name: "deepseek_caching".to_string(),
            user_id: "user_1".to_string(),
            session_id: None,
            state: std::collections::HashMap::new(),
        })
        .await?;

    let session_id = session.id().to_string();

    let runner = Runner::new(RunnerConfig {
        app_name: "deepseek_caching".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
    })?;

    println!("=== DeepSeek Context Caching Demo ===\n");
    println!("This demo shows how DeepSeek caches the document context.");
    println!("The first request processes all tokens (cache miss).");
    println!("Subsequent requests reuse the cached prefix (cache hit = 10x cheaper!).\n");
    println!(
        "Document length: ~{} words, ~{} tokens (estimated)\n",
        DOCUMENT.split_whitespace().count(),
        DOCUMENT.split_whitespace().count() * 4 / 3
    );

    // Question 1 - First request (cache miss expected)
    println!("=== Question 1 (First request - cache population) ===\n");
    let q1 = "What are the core working hours?";
    println!("User: {}\n", q1);

    let content = Content::new("user").with_text(q1);
    let mut stream = runner.run("user_1".to_string(), session_id.clone(), content).await?;

    print!("Assistant: ");
    while let Some(event) = stream.next().await {
        if let Ok(e) = event
            && let Some(content) = e.llm_response.content
        {
            for part in content.parts {
                if let adk_core::Part::Text { text } = part {
                    print!("{}", text);
                }
            }
        }
    }
    println!("\n");

    // Question 2 - Should get cache hits
    println!("=== Question 2 (Cache should be populated now) ===\n");
    let q2 = "How much can I get reimbursed for internet service?";
    println!("User: {}\n", q2);

    let content = Content::new("user").with_text(q2);
    let mut stream = runner.run("user_1".to_string(), session_id.clone(), content).await?;

    print!("Assistant: ");
    while let Some(event) = stream.next().await {
        if let Ok(e) = event
            && let Some(content) = e.llm_response.content
        {
            for part in content.parts {
                if let adk_core::Part::Text { text } = part {
                    print!("{}", text);
                }
            }
        }
    }
    println!("\n");

    // Question 3 - Should also get cache hits
    println!("=== Question 3 (More cache hits expected) ===\n");
    let q3 = "What equipment does the company provide for remote work?";
    println!("User: {}\n", q3);

    let content = Content::new("user").with_text(q3);
    let mut stream = runner.run("user_1".to_string(), session_id.clone(), content).await?;

    print!("Assistant: ");
    while let Some(event) = stream.next().await {
        if let Ok(e) = event
            && let Some(content) = e.llm_response.content
        {
            for part in content.parts {
                if let adk_core::Part::Text { text } = part {
                    print!("{}", text);
                }
            }
        }
    }
    println!("\n");

    println!("=== Cache Benefit Summary ===");
    println!("• The document prefix (~1000 tokens) was cached after the first request");
    println!("• Questions 2 and 3 benefited from cache hits");
    println!("• Cache hit tokens cost 0.1 yuan/million vs 1 yuan/million for cache miss");
    println!("• That's a 10x cost reduction for repeated context!\n");
    println!("Note: Check your DeepSeek dashboard for actual cache hit/miss metrics.");

    Ok(())
}
