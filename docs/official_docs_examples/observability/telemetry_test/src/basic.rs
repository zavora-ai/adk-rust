//! Basic Telemetry Example
//!
//! Demonstrates structured logging with adk-telemetry.
//!
//! Run:
//!   cd doc-test/observability/telemetry_test
//!   cargo run --bin basic
//!
//! With debug logging:
//!   RUST_LOG=debug cargo run --bin basic

use adk_telemetry::{debug, error, info, init_telemetry, instrument, trace, warn};

#[tokio::main]
async fn main() {
    // Initialize telemetry first
    init_telemetry("telemetry-example").expect("Failed to init telemetry");

    println!("Telemetry Basic Example");
    println!("=======================\n");

    // Basic logging at different levels
    info!("Application started");
    debug!("Debug message (visible with RUST_LOG=debug)");
    trace!("Trace message (visible with RUST_LOG=trace)");

    // Structured logging with fields
    info!(
        agent.name = "my_agent",
        session.id = "sess-123",
        user.id = "user-456",
        "Processing user request"
    );

    // Call instrumented function
    process_request("user-789", "Hello, agent!").await;

    // Warning and error examples
    warn!(rate_limit = 95, "Rate limit approaching");

    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "Resource not found");
    error!(error = ?err, "Operation failed");

    info!("Application completed");

    println!("\n✓ Check the logs above for structured output!");
}

#[instrument]
async fn process_request(user_id: &str, message: &str) {
    info!("Processing request");

    // Simulate some work
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Nested instrumented call
    validate_input(message).await;

    info!("Request processed successfully");
}

#[instrument(skip(input))] // Skip sensitive data
async fn validate_input(input: &str) {
    debug!(input_length = input.len(), "Validating input");
    // Validation logic
}
