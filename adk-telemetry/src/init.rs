//! Telemetry initialization and configuration

use std::sync::{Arc, Once};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::span_exporter::{AdkSpanExporter, AdkSpanLayer};

static INIT: Once = Once::new();

/// Error returned by telemetry initialization functions.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// Failed to build the tracing/OTLP pipeline.
    #[error("telemetry init failed: {0}")]
    Init(String),
}

/// Initialize basic telemetry with console logging.
///
/// # Arguments
/// * `service_name` - Name of the service for trace identification
///
/// # Example
/// ```
/// use adk_telemetry::init_telemetry;
/// init_telemetry("my-agent-service").expect("Failed to initialize telemetry");
/// ```
pub fn init_telemetry(service_name: &str) -> Result<(), TelemetryError> {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap_or_else(|_| EnvFilter::new("info"));

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_line_number(true),
            )
            .init();

        tracing::info!(service.name = service_name, "telemetry initialized");
    });

    Ok(())
}

/// Initialize telemetry with OpenTelemetry OTLP export.
///
/// Enables distributed tracing by exporting spans to an OTLP collector.
///
/// # Arguments
/// * `service_name` - Name of the service for trace identification
/// * `endpoint` - OTLP collector endpoint (e.g., "http://localhost:4317")
///
/// # Example
/// ```no_run
/// use adk_telemetry::init_with_otlp;
/// init_with_otlp("my-agent", "http://localhost:4317")
///     .expect("Failed to initialize telemetry");
/// ```
pub fn init_with_otlp(service_name: &str, endpoint: &str) -> Result<(), TelemetryError> {
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_otlp::WithExportConfig;
    use tracing_opentelemetry::OpenTelemetryLayer;

    let endpoint = endpoint.to_string();
    let service_name = service_name.to_string();

    let init_error: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    INIT.call_once(|| {
        let resource = opentelemetry_sdk::Resource::builder_empty()
            .with_attributes([opentelemetry::KeyValue::new("service.name", service_name.clone())])
            .build();

        // Build OTLP span exporter
        let span_exporter = match opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
        {
            Ok(e) => e,
            Err(e) => {
                *init_error.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(format!("failed to build OTLP span exporter: {e}"));
                return;
            }
        };

        // Build tracer provider with batch exporter
        let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();

        let tracer = tracer_provider.tracer("adk-telemetry");
        let _ = opentelemetry::global::set_tracer_provider(tracer_provider);

        // Initialize metrics
        let metric_exporter = match opentelemetry_otlp::MetricExporter::builder()
            .with_tonic()
            .with_endpoint(&endpoint)
            .build()
        {
            Ok(e) => e,
            Err(e) => {
                *init_error.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(format!("failed to build OTLP metric exporter: {e}"));
                return;
            }
        };

        let meter_provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource)
            .build();

        opentelemetry::global::set_meter_provider(meter_provider);

        let telemetry_layer = OpenTelemetryLayer::new(tracer);

        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap_or_else(|_| EnvFilter::new("info"));

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_line_number(true),
            )
            .with(telemetry_layer)
            .init();

        tracing::info!(
            service.name = service_name,
            otlp.endpoint = %endpoint,
            "telemetry initialized with OpenTelemetry"
        );
    });

    if let Some(err) = init_error.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(TelemetryError::Init(err));
    }

    Ok(())
}

/// Shutdown telemetry and flush any pending spans.
///
/// Should be called before application exit to ensure all telemetry data is sent.
/// In OTel 0.28+, the tracer provider is shut down when the last reference is dropped.
/// This function is kept for backward compatibility and explicitly drops the global provider.
pub fn shutdown_telemetry() {
    // In OTel 0.28, shutdown_tracer_provider() was removed.
    // The SdkTracerProvider shuts down automatically when the last reference is dropped.
    // We trigger this by replacing the global provider with a no-op, which drops the old one.
    let _ = opentelemetry::global::set_tracer_provider(
        opentelemetry::trace::noop::NoopTracerProvider::new(),
    );
}

/// Initialize telemetry with ADK span exporter.
///
/// Creates a shared span exporter that can be used by both telemetry and the debug API.
/// Returns the exporter so it can be passed to the debug controller.
pub fn init_with_adk_exporter(service_name: &str) -> Result<Arc<AdkSpanExporter>, TelemetryError> {
    let exporter = Arc::new(AdkSpanExporter::new());
    let exporter_clone = exporter.clone();

    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap_or_else(|_| EnvFilter::new("info"));

        let adk_layer = AdkSpanLayer::new(exporter_clone);

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_line_number(true),
            )
            .with(adk_layer)
            .init();

        tracing::info!(service.name = service_name, "telemetry initialized with ADK span exporter");
    });

    Ok(exporter)
}
