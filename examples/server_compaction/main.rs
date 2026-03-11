//! Server Compaction Configuration Example
//!
//! Demonstrates configuring context compaction through `ServerConfig` so that
//! long-running agent sessions automatically summarize older events. This
//! feature was added by the browser production hardening spec.
//!
//! ## Features Showcased
//!
//! - `ServerConfig::with_compaction()` — configure compaction at the server level
//! - `EventsCompactionConfig` — compaction interval, overlap, and summarizer
//! - Compaction config flows through RuntimeController and A2A to RunnerConfig
//!
//! ## Running
//!
//! ```bash
//! cargo run --example server_compaction
//! ```

use adk_core::{
    Agent, BaseEventsSummarizer, Event, EventStream, EventsCompactionConfig, InvocationContext,
    Result as AdkResult, SingleAgentLoader,
};
use adk_server::{SecurityConfig, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use futures::stream;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Minimal agent for demonstration
// ---------------------------------------------------------------------------

struct DemoAgent;

#[async_trait]
impl Agent for DemoAgent {
    fn name(&self) -> &str {
        "demo_agent"
    }
    fn description(&self) -> &str {
        "A demo agent for compaction config"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(stream::empty()))
    }
}

// ---------------------------------------------------------------------------
// Custom event summarizer
// ---------------------------------------------------------------------------

struct CustomSummarizer;

#[async_trait]
impl BaseEventsSummarizer for CustomSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> AdkResult<Option<Event>> {
        // In production, this would call an LLM to summarize the events.
        // For this example, we create a simple summary.
        if events.is_empty() {
            return Ok(None);
        }
        let summary_text =
            format!("Summary of {} events: conversation covered various topics.", events.len());
        let mut summary_event = Event::new("model");
        summary_event.llm_response.content = Some(adk_core::Content {
            role: "model".to_string(),
            parts: vec![adk_core::Part::Text { text: summary_text }],
        });
        Ok(Some(summary_event))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Server Compaction Configuration Example ===\n");

    let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(DemoAgent)));
    let session_service = Arc::new(InMemorySessionService::new());

    // =========================================================================
    // Example 1: ServerConfig without compaction (default — backward compat)
    // =========================================================================
    println!("--- Example 1: Default ServerConfig (no compaction) ---");

    let config_default = ServerConfig::new(agent_loader.clone(), session_service.clone());

    println!("  compaction_config: {:?}", config_default.compaction_config.is_some());
    println!("  (None — preserves current behavior, no automatic summarization)\n");

    // =========================================================================
    // Example 2: ServerConfig with compaction enabled
    // =========================================================================
    println!("--- Example 2: ServerConfig with compaction ---");

    let compaction = EventsCompactionConfig {
        // Compact after every 10 events
        compaction_interval: 10,
        // Keep 2 events of overlap for context continuity
        overlap_size: 2,
        // Custom summarizer (in production, use LlmEventSummarizer)
        summarizer: Arc::new(CustomSummarizer),
    };

    let config_with_compaction = ServerConfig::new(agent_loader.clone(), session_service.clone())
        .with_compaction(compaction);

    let cc = config_with_compaction.compaction_config.as_ref().unwrap();
    println!("  compaction_interval: {}", cc.compaction_interval);
    println!("  overlap_size: {}", cc.overlap_size);
    println!("  summarizer: configured");
    println!("  → This config flows through RuntimeController and A2A to RunnerConfig\n");

    // =========================================================================
    // Example 3: Full production ServerConfig
    // =========================================================================
    println!("--- Example 3: Full production ServerConfig ---");

    let production_config = ServerConfig::new(agent_loader.clone(), session_service.clone())
        .with_compaction(EventsCompactionConfig {
            compaction_interval: 20,
            overlap_size: 3,
            summarizer: Arc::new(CustomSummarizer),
        })
        .with_security(SecurityConfig::production(vec!["https://app.example.com".into()]))
        .with_backend_url("http://0.0.0.0:8080");

    println!("  compaction: enabled (interval=20, overlap=3)");
    println!("  security: production (CORS restricted)");
    println!("  backend_url: {}", production_config.backend_url.as_deref().unwrap_or("none"));
    println!("  error_details: {}", production_config.security.expose_error_details);

    // =========================================================================
    // Example 4: Using LlmEventSummarizer (the production summarizer)
    // =========================================================================
    println!("\n--- Example 4: LlmEventSummarizer (production path) ---");
    println!("  In production, use adk_agent::LlmEventSummarizer with a real LLM:");
    println!();
    println!("    use adk_agent::LlmEventSummarizer;");
    println!("    use adk_model::GeminiModel;");
    println!();
    println!("    let model = Arc::new(GeminiModel::new(&api_key, \"gemini-2.5-flash\")?);");
    println!("    let summarizer = Arc::new(LlmEventSummarizer::new(model));");
    println!();
    println!("    let config = ServerConfig::new(loader, sessions)");
    println!("        .with_compaction(EventsCompactionConfig {{");
    println!("            compaction_interval: 20,");
    println!("            overlap_size: 3,");
    println!("            summarizer,");
    println!("        }});");

    println!("\n=== Example Complete ===");
    Ok(())
}
