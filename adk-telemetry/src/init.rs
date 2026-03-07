//! Telemetry initialization and configuration

use std::sync::{Arc, Once};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::span_exporter::{AdkSpanExporter, AdkSpanLayer};

static INIT: Once = Once::new();

/// Configuration for the telemetry system
#[derive(Debug, Clone, Default)]
pub struct TelemetryConfig {
    pub service_name: String,
    pub otlp_endpoint: Option<String>,
    pub adk_exporter: bool,
    pub langsmith_project: Option<String>,
    pub default_level: Option<String>,
    pub log_directives: Vec<String>,
}

impl TelemetryConfig {
    /// Create a new configuration with the given service name.
    pub fn new(service_name: impl Into<String>) -> Self {
        Self { service_name: service_name.into(), ..Default::default() }
    }

    /// Enable OTLP export to the specified endpoint.
    pub fn with_otlp(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// Enable the ADK-style span exporter.
    pub fn with_adk_exporter(mut self) -> Self {
        self.adk_exporter = true;
        self
    }

    /// Set the default log level (e.g., "debug", "info").
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.default_level = Some(level.into());
        self
    }

    /// Add a custom tracing directive (e.g., "my_crate=debug").
    pub fn with_log_directive(mut self, directive: impl Into<String>) -> Self {
        self.log_directives.push(directive.into());
        self
    }

    /// Enable LangSmith support for the specified project.
    #[cfg(feature = "langsmith")]
    pub fn with_langsmith(mut self, project: impl Into<String>) -> Self {
        self.langsmith_project = Some(project.into());
        self
    }

    /// Load configuration from environment variables.
    ///
    /// Supported variables:
    /// - `SERVICE_NAME`: The name of the service (default: "adk-service")
    /// - `OTLP_ENDPOINT`: OTLP collector endpoint (e.g., "http://localhost:4317")
    /// - `ADK_TELEMETRY_EXPORTER`: Set to "true" to enable ADK-style spans
    /// - `LANGSMITH_PROJECT`: LangSmith project name (enables LangSmith layer)
    /// - `LOG_LEVEL`: Default log level (default: "info")
    pub fn from_env() -> Self {
        let service_name =
            std::env::var("SERVICE_NAME").unwrap_or_else(|_| "adk-service".to_string());
        let otlp_endpoint = std::env::var("OTLP_ENDPOINT").ok();
        let adk_exporter =
            std::env::var("ADK_TELEMETRY_EXPORTER").map(|v| v == "true").unwrap_or(false);
        let langsmith_project = std::env::var("LANGSMITH_PROJECT").ok();
        let default_level = std::env::var("LOG_LEVEL").ok();

        Self {
            service_name,
            otlp_endpoint,
            adk_exporter,
            langsmith_project,
            default_level,
            log_directives: Vec::new(),
        }
    }
}

/// Initialize telemetry with basic console logging
pub fn init_telemetry(service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    init_with_config(TelemetryConfig::new(service_name))
}

/// Initialize telemetry with OpenTelemetry OTLP export
pub fn init_with_otlp(
    service_name: &str,
    endpoint: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    init_with_config(TelemetryConfig::new(service_name).with_otlp(endpoint))
}

/// Shutdown telemetry and flush any pending spans
pub fn shutdown_telemetry() {
    opentelemetry::global::shutdown_tracer_provider();
}

/// Initialize telemetry with ADK-Go style span exporter
pub fn init_with_adk_exporter(
    service_name: &str,
) -> Result<Arc<AdkSpanExporter>, Box<dyn std::error::Error>> {
    let exporter = Arc::new(AdkSpanExporter::new());
    let config = TelemetryConfig::new(service_name).with_adk_exporter();

    // Internal helper to bypass normal init for pre-created exporter
    init_internal(config, Some(exporter.clone()))?;

    Ok(exporter)
}

/// Initialize telemetry with LangSmith support
#[cfg(feature = "langsmith")]
pub fn init_with_langsmith(
    service_name: &str,
    project_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    init_with_config(TelemetryConfig::new(service_name).with_langsmith(project_name))
}

/// Unified initialization function that enables multiple backends
pub fn init_with_config(config: TelemetryConfig) -> Result<(), Box<dyn std::error::Error>> {
    init_internal(config, None)
}

fn init_internal(
    config: TelemetryConfig,
    pre_created_exporter: Option<Arc<AdkSpanExporter>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tracing_subscriber::fmt;

    INIT.call_once(|| {
        let mut filter = EnvFilter::try_from_default_env()
            .or_else(|_| {
                let level = config.default_level.as_deref().unwrap_or("info");
                EnvFilter::try_new(level)
            })
            .expect("Failed to create env filter");

        for directive in &config.log_directives {
            filter = filter.add_directive(directive.parse().expect("Invalid log directive"));
        }

        // Always add fmt (console) layer
        let fmt_layer = fmt::layer().with_target(true).with_thread_ids(true).with_line_number(true);

        // OTLP Layer
        #[cfg(not(target_arch = "wasm32"))]
        let otlp_layer = if let Some(endpoint) = &config.otlp_endpoint {
            use opentelemetry_otlp::WithExportConfig;
            use tracing_opentelemetry::OpenTelemetryLayer;

            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_exporter(opentelemetry_otlp::new_exporter().tonic().with_endpoint(endpoint))
                .with_trace_config(opentelemetry_sdk::trace::config().with_resource(
                    opentelemetry_sdk::Resource::new(vec![opentelemetry::KeyValue::new(
                        "service.name",
                        config.service_name.clone(),
                    )]),
                ))
                .install_batch(opentelemetry_sdk::runtime::Tokio)
                .expect("Failed to install OTLP pipeline");

            let meter_provider = opentelemetry_otlp::new_pipeline()
                .metrics(opentelemetry_sdk::runtime::Tokio)
                .with_exporter(opentelemetry_otlp::new_exporter().tonic().with_endpoint(endpoint))
                .with_resource(opentelemetry_sdk::Resource::new(vec![
                    opentelemetry::KeyValue::new("service.name", config.service_name.clone()),
                ]))
                .build()
                .expect("Failed to build meter provider");

            opentelemetry::global::set_meter_provider(meter_provider);
            Some(OpenTelemetryLayer::new(tracer))
        } else {
            None
        };

        // ADK Layer
        let adk_layer = if config.adk_exporter || pre_created_exporter.is_some() {
            let exporter = pre_created_exporter.unwrap_or_else(|| Arc::new(AdkSpanExporter::new()));
            Some(AdkSpanLayer::new(exporter))
        } else {
            None
        };

        // LangSmith Layer
        #[cfg(feature = "langsmith")]
        let langsmith_layer = if let Some(_project) = &config.langsmith_project {
            use crate::langsmith::LangSmithLayer;
            langsmith_rust::init();
            Some(LangSmithLayer::new())
        } else {
            None
        };

        let registry = tracing_subscriber::registry().with(filter).with(fmt_layer);

        #[cfg(not(target_arch = "wasm32"))]
        let registry = registry.with(otlp_layer);

        let registry = registry.with(adk_layer);

        #[cfg(feature = "langsmith")]
        let registry = registry.with(langsmith_layer);

        registry.init();

        tracing::info!(
            service.name = config.service_name,
            otlp.enabled = config.otlp_endpoint.is_some(),
            adk.enabled = config.adk_exporter,
            langsmith.enabled = config.langsmith_project.is_some(),
            log.level = config.default_level.as_deref().unwrap_or("env"),
            "Telemetry system initialized"
        );
    });

    Ok(())
}
