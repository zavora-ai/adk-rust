//! # ADK Telemetry
//!
//! Production-grade observability for ADK using structured logging and distributed tracing.
//!
//! ## Features
//! - Structured logging with `tracing`
//! - OpenTelemetry integration for distributed tracing
//! - OTLP export for observability backends (Jaeger, Datadog, etc.)
//! - Automatic context propagation
//! - OTel GenAI Semantic Conventions (v1.41.0) via `genai-semconv` feature (enabled by default)
//!
//! ## Usage
//!
//! ```rust
//! use adk_telemetry::{init_telemetry, info, instrument};
//!
//! fn main() -> Result<(), adk_telemetry::TelemetryError> {
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
pub mod span_exporter;
pub mod spans;

// Direct span export to a local SQLite file (feature-gated)
#[cfg(feature = "sqlite")]
pub mod sqlite;

// GenAI Semantic Conventions module (feature-gated)
#[cfg(feature = "genai-semconv")]
pub mod semconv;

// Content event configuration (feature-gated)
#[cfg(feature = "genai-semconv")]
pub mod config;

// Content event emitter (feature-gated)
#[cfg(feature = "genai-semconv")]
pub mod events;

// Re-export tracing macros for convenience
pub use tracing::{Span, debug, error, info, instrument, trace, warn};

// Re-export span helpers
pub use spans::*;

// Re-export span exporter (ADK-Go style)
pub use span_exporter::*;

// Re-export init functions and error type
#[cfg(feature = "sqlite")]
pub use init::init_with_sqlite;
pub use init::{TelemetryError, init_telemetry, init_with_adk_exporter, shutdown_telemetry};
#[cfg(feature = "otlp")]
pub use init::{build_otlp_layer, init_with_otlp};
#[cfg(feature = "sqlite")]
pub use sqlite::{SessionSummary, SpanRow, SqliteSpanExporter, SqliteTraceReader};

// Re-export metrics
#[cfg(feature = "otlp")]
pub use opentelemetry::global;
#[cfg(feature = "otlp")]
pub use opentelemetry::metrics::{Meter, MeterProvider};

// Re-export key semconv types for convenience
#[cfg(feature = "genai-semconv")]
pub use semconv::{GenAiOperation, GenAiProvider, GenAiResponseRecorder, GenAiSpanBuilder};
