//! # ADK Telemetry
//!
//! Production-grade observability for ADK using structured logging and distributed tracing.
//!
//! ## Features
//! - Structured logging with `tracing`
//! - OpenTelemetry integration for distributed tracing
//! - OTLP export for observability backends (Jaeger, Datadog, etc.)
//! - Automatic context propagation
//!
//! ## Usage
//!
//! ```rust
//! use adk_telemetry::{init_telemetry, info, instrument};
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize telemetry in your main
//!     init_telemetry("my-service")?;
//!
//!     // Use logging macros
//!     #[instrument]
//!     async fn my_function() {
//!         info!("Function called");
//!     }
//!     Ok(())
//! }
//! ```

pub mod init;
pub mod spans;

// Re-export tracing macros for convenience
pub use tracing::{debug, error, info, trace, warn, instrument, Span};

// Re-export span helpers
pub use spans::*;

// Re-export init functions
pub use init::{init_telemetry, init_with_otlp, shutdown_telemetry};

// Re-export metrics
pub use opentelemetry::metrics::{Meter, MeterProvider};
pub use opentelemetry::global;
