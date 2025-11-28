//! Telemetry initialization and configuration

use std::sync::Once;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

static INIT: Once = Once::new();

/// Initialize basic telemetry with console logging
///
/// # Arguments
/// * `service_name` - Name of the service for trace identification
///
/// # Example
/// ```
/// use adk_telemetry::init_telemetry;
/// init_telemetry("my-agent-service").expect("Failed to initialize telemetry");
/// ```
pub fn init_telemetry(service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .expect("Failed to create env filter");

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_line_number(true)
            )
            .init();

        tracing::info!(service.name = service_name, "Telemetry initialized");
    });

    Ok(())
}

/// Initialize telemetry with OpenTelemetry OTLP export
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
pub fn init_with_otlp(
    service_name: &str,
    endpoint: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use opentelemetry_otlp::WithExportConfig;
    use tracing_opentelemetry::OpenTelemetryLayer;

    INIT.call_once(|| {
        // install_batch returns a Tracer directly
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint)
            )
            .with_trace_config(
                opentelemetry_sdk::trace::config()
                    .with_resource(opentelemetry_sdk::Resource::new(vec![
                        opentelemetry::KeyValue::new("service.name", service_name.to_string()),
                    ]))
            )
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .expect("Failed to install OTLP pipeline");

        // Initialize metrics
        let meter_provider = opentelemetry_otlp::new_pipeline()
            .metrics(opentelemetry_sdk::runtime::Tokio)
            .with_exporter(
                opentelemetry_otlp::new_exporter()
                    .tonic()
                    .with_endpoint(endpoint)
            )
            .with_resource(opentelemetry_sdk::Resource::new(vec![
                opentelemetry::KeyValue::new("service.name", service_name.to_string()),
            ]))
            .build()
            .expect("Failed to build meter provider");

        opentelemetry::global::set_meter_provider(meter_provider);

        let telemetry_layer = OpenTelemetryLayer::new(tracer);

        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .expect("Failed to create env filter");

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(true)
                    .with_thread_ids(true)
                    .with_line_number(true)
            )
            .with(telemetry_layer)
            .init();

        tracing::info!(
            service.name = service_name,
            otlp.endpoint = endpoint,
            "Telemetry initialized with OpenTelemetry"
        );
    });

    Ok(())
}

/// Shutdown telemetry and flush any pending spans
///
/// Should be called before application exit to ensure all telemetry data is sent.
pub fn shutdown_telemetry() {
    opentelemetry::global::shutdown_tracer_provider();
}
