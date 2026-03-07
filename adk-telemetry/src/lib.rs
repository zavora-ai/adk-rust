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
#[cfg(feature = "langsmith")]
pub mod langsmith;
pub mod span_exporter;
pub mod spans;
pub mod trace_context;
pub mod visitor;

// Re-export tracing macros for convenience
pub use tracing::{Span, debug, error, info, instrument, trace, warn};

// Re-export span helpers
pub use spans::*;
pub use trace_context::TraceContextExt;

// Re-export span exporter (ADK-Go style)
pub use span_exporter::*;

// Re-export init functions
#[cfg(feature = "langsmith")]
pub use init::init_with_langsmith;
pub use init::{init_telemetry, init_with_adk_exporter, init_with_otlp, shutdown_telemetry};

// Re-export metrics
pub use opentelemetry::global;
pub use opentelemetry::metrics::{Meter, MeterProvider};
