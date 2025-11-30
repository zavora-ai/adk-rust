//! Validates: docs/official_docs/observability/telemetry.md
//!
//! This example demonstrates telemetry setup and usage in ADK-Rust.
//! It shows structured logging, instrumentation, and pre-configured spans.
//!
//! Run modes:
//!   cargo run --example telemetry -p adk-rust-guide              # Validation mode (default)
//!   cargo run --example telemetry -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example telemetry -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::runner::{Runner, RunnerConfig};
use adk_rust::session::{CreateRequest, InMemorySessionService, SessionService};
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use adk_telemetry::{
    add_context_attributes, agent_run_span, callback_span, debug, error, info, init_telemetry,
    model_call_span, tool_execute_span, warn,
};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::instrument;

/// Example instrumented function
#[instrument]
async fn instrumented_operation(operation_name: &str, count: u32) {
    info!(
        operation = operation_name,
        count = count,
        "Performing instrumented operation"
    );

    // Simulate some work
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    debug!(operation = operation_name, "Operation details logged");
}

/// Example function with sensitive parameter skipping
#[instrument(skip(api_key))]
async fn secure_operation(api_key: &str, user_id: &str) {
    info!(user_id = user_id, "Performing secure operation");
    // api_key won't appear in traces
    let _ = api_key; // Use the parameter to avoid warnings
}

/// Custom tool with telemetry
#[instrument(skip(_ctx))]
async fn telemetry_demo_tool(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
    let span = tool_execute_span("telemetry_demo_tool");
    let _enter = span.enter();

    let operation = args["operation"].as_str().unwrap_or("default");
    info!(operation = operation, "Tool execution started");

    // Add context attributes
    add_context_attributes("demo-user", "demo-session");

    // Demonstrate different log levels
    debug!(operation = operation, "Debug information");
    info!(operation = operation, "Processing operation");

    // Simulate work
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let result = json!({
        "status": "success",
        "operation": operation,
        "message": "Telemetry demo tool executed successfully"
    });

    info!(
        operation = operation,
        result = ?result,
        "Tool execution completed"
    );

    Ok(result)
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Initialize telemetry first (matches documentation)
    init_telemetry("telemetry-demo-service")?;

    info!("Telemetry example starting");

    // Load API key
    let api_key = init_env();

    // Demonstrate structured logging with fields
    info!(
        service = "telemetry-demo",
        version = "1.0.0",
        "Service initialized"
    );

    // Demonstrate different log levels
    debug!("Debug level logging");
    info!("Info level logging");
    warn!("Warning level logging");

    // Demonstrate instrumented functions
    instrumented_operation("demo_op", 42).await;
    secure_operation(&api_key, "user-123").await;

    // Create model with telemetry
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    info!(model = "gemini-2.0-flash-exp", "Model created");

    // Create custom tool with telemetry
    let demo_tool = FunctionTool::new(
        "telemetry_demo",
        "A demo tool that showcases telemetry features",
        telemetry_demo_tool,
    );

    // Build agent with telemetry-enabled callbacks
    let agent = LlmAgentBuilder::new("telemetry_agent")
        .description("An agent demonstrating telemetry features")
        .instruction("You are a helpful assistant that demonstrates telemetry. When asked, use the telemetry_demo tool.")
        .model(Arc::new(model))
        .tool(Arc::new(demo_tool))
        // Before agent callback with telemetry
        .before_callback(Box::new(|ctx| {
            Box::pin(async move {
                let span = callback_span("before_agent");
                let _enter = span.enter();

                info!(
                    agent.name = ctx.agent_name(),
                    user.id = ctx.user_id(),
                    session.id = ctx.session_id(),
                    invocation.id = ctx.invocation_id(),
                    "Agent execution starting"
                );

                Ok(None)
            })
        }))
        // After agent callback with telemetry
        .after_callback(Box::new(|ctx| {
            Box::pin(async move {
                let span = callback_span("after_agent");
                let _enter = span.enter();

                info!(
                    agent.name = ctx.agent_name(),
                    invocation.id = ctx.invocation_id(),
                    "Agent execution completed"
                );

                Ok(None)
            })
        }))
        // Before model callback with telemetry
        .before_model_callback(Box::new(|ctx, request| {
            Box::pin(async move {
                let span = model_call_span("gemini-2.0-flash-exp");
                let _enter = span.enter();

                info!(
                    agent.name = ctx.agent_name(),
                    content_count = request.contents.len(),
                    "Model call starting"
                );

                Ok(BeforeModelResult::Continue(request))
            })
        }))
        // After model callback with telemetry
        .after_model_callback(Box::new(|ctx, response| {
            Box::pin(async move {
                info!(
                    agent.name = ctx.agent_name(),
                    has_content = response.content.is_some(),
                    "Model call completed"
                );

                Ok(None)
            })
        }))
        .build()?;

    if is_interactive_mode() {
        info!("Starting interactive mode");
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode
        print_validating("observability/telemetry.md");

        // Demonstrate agent execution span
        let span = agent_run_span("telemetry_agent", "validation-inv-001");
        let _enter = span.enter();

        info!("Running validation test");

        // Create session service for runner
        let session_service = Arc::new(InMemorySessionService::new());

        // Create session first
        let initial_state = HashMap::new();
        session_service
            .create(CreateRequest {
                app_name: "telemetry_demo".to_string(),
                user_id: "demo_user".to_string(),
                session_id: Some("demo_session".to_string()),
                state: initial_state,
            })
            .await?;

        // Create a simple runner for validation
        let runner = Runner::new(RunnerConfig {
            app_name: "telemetry_demo".to_string(),
            agent: Arc::new(agent),
            session_service: session_service.clone(),
            artifact_service: None,
            memory_service: None,
        })?;

        // Test with a simple message
        let input = Content::new("user").with_text("Hello! Can you tell me about telemetry?");

        let mut stream = runner
            .run(
                "demo_user".to_string(),
                "demo_session".to_string(),
                input,
            )
            .await?;

        // Collect the response
        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(event) => {
                    if let Some(content) = &event.llm_response.content {
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                debug!(response_text = text, "Received response");
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "Agent execution failed");
                    return Err(e.into());
                }
            }
        }

        info!("Agent execution successful");

        println!("\n✅ Telemetry features demonstrated:");
        println!("  - Structured logging with fields");
        println!("  - Different log levels (trace, debug, info, warn, error)");
        println!("  - Function instrumentation with #[instrument]");
        println!("  - Pre-configured spans (agent_run_span, model_call_span, tool_execute_span)");
        println!("  - Callback spans");
        println!("  - Context attributes (user_id, session_id)");
        println!("  - Sensitive parameter skipping");

        print_success("telemetry");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example telemetry -p adk-rust-guide -- chat");
        println!("\nNote: Set RUST_LOG environment variable to control log levels:");
        println!("  RUST_LOG=debug cargo run --example telemetry -p adk-rust-guide");
    }

    Ok(())
}
