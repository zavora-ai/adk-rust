//! Telemetry to Google Cloud.
//!
//! Three integration points, composable or standalone:
//!
//! - [`init_with_gcp`] — OTLP trace export straight to
//!   `https://telemetry.googleapis.com` with `Authorization: Bearer` headers
//!   minted from Application Default Credentials, plus Cloud Logging JSON
//!   output on stdout.
//! - [`init_json_logging`] — Cloud Logging structured JSON on stdout only
//!   (severity mapping and trace correlation, no exporter).
//! - [`gcp_resource_attributes`] — GCP resource detection from the environment
//!   variables the platform sets in deployed containers.
//!
//! For environments without direct API access, export to a collector sidecar
//! instead: `init_with_otlp("my-agent", "http://localhost:4317")` — see
//! `docs/official_docs/observability/gcp.md`.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use opentelemetry::KeyValue;
use tonic::metadata::{AsciiMetadataKey, AsciiMetadataValue};
use tracing_subscriber::{
    EnvFilter,
    layer::{Layer, SubscriberExt},
    util::SubscriberInitExt,
};

use crate::init::{INIT, TelemetryError, otlp_pipeline};

/// Google Cloud OTLP ingest endpoint (Telemetry API, gRPC over HTTPS).
pub const GCP_OTLP_ENDPOINT: &str = "https://telemetry.googleapis.com";

const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const USER_PROJECT_HEADER: &str = "x-goog-user-project";

/// Refresh cadence for the auth headers injected into exporter requests.
/// `google-cloud-auth` caches tokens internally and only re-mints near expiry,
/// so a short cadence costs nothing between refreshes.
const HEADER_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Project ID of the Google Cloud project receiving telemetry.
const ENV_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// Service name set by Cloud Run and other Knative-shaped runtimes.
const ENV_K_SERVICE: &str = "K_SERVICE";
/// Bare numeric engine ID set by Vertex AI Agent Engine in deployed containers.
const ENV_AGENT_ENGINE_ID: &str = "GOOGLE_CLOUD_AGENT_ENGINE_ID";

/// Cloud Logging structured-log field parsed for trace correlation.
const LOG_TRACE_KEY: &str = "logging.googleapis.com/trace";
const LOG_SPAN_ID_KEY: &str = "logging.googleapis.com/spanId";
const LOG_TRACE_SAMPLED_KEY: &str = "logging.googleapis.com/trace_sampled";

/// Supplies Google Cloud authorization headers for the OTLP exporter.
///
/// Wraps [`google_cloud_auth`] credentials and appends the
/// `x-goog-user-project` header naming the project that receives (and is
/// billed for) the telemetry.
///
/// # Example
/// ```no_run
/// use adk_telemetry::GcpHeaderSupplier;
///
/// # async fn example() -> Result<(), adk_telemetry::TelemetryError> {
/// let supplier = GcpHeaderSupplier::new_with_adc("my-project")?;
/// let headers = supplier.headers().await?;
/// assert!(headers.iter().any(|(name, _)| name == "authorization"));
/// # Ok(())
/// # }
/// ```
pub struct GcpHeaderSupplier {
    credentials: Credentials,
    user_project: String,
}

impl GcpHeaderSupplier {
    /// Creates a supplier using Application Default Credentials (ADC).
    ///
    /// # Errors
    /// Returns [`TelemetryError::Init`] when ADC cannot be constructed — for
    /// example when no credential source is available. Run
    /// `gcloud auth application-default login` locally, or attach a service
    /// account in deployed environments.
    pub fn new_with_adc(user_project: impl Into<String>) -> Result<Self, TelemetryError> {
        let credentials = credentials::Builder::default()
            .with_scopes([CLOUD_PLATFORM_SCOPE])
            .build()
            .map_err(|e| {
                TelemetryError::Init(format!(
                    "failed to build application default credentials for gcp telemetry: {e}. \
                     Run `gcloud auth application-default login` or attach a service account."
                ))
            })?;
        Ok(Self::with_credentials(credentials, user_project))
    }

    /// Creates a supplier with explicit credentials.
    pub fn with_credentials(credentials: Credentials, user_project: impl Into<String>) -> Self {
        Self { credentials, user_project: user_project.into() }
    }

    /// Mints the current header set: the credential headers (typically
    /// `authorization: Bearer …`) plus `x-goog-user-project`.
    ///
    /// # Errors
    /// Returns [`TelemetryError::Init`] when the credential provider fails or
    /// returns a non-printable header value.
    pub async fn headers(&self) -> Result<Vec<(String, String)>, TelemetryError> {
        let cacheable = self.credentials.headers(http::Extensions::new()).await.map_err(|e| {
            TelemetryError::Init(format!("failed to mint gcp telemetry auth headers: {e}"))
        })?;

        let header_map = match cacheable {
            CacheableResource::New { data, .. } => data,
            // Unreachable with empty extensions (no entity tag was offered),
            // kept as an error instead of a panic in case that changes upstream.
            CacheableResource::NotModified => {
                return Err(TelemetryError::Init(
                    "gcp credentials returned NotModified without a prior cached header set"
                        .to_string(),
                ));
            }
        };

        let mut headers = Vec::with_capacity(header_map.len() + 1);
        for (name, value) in &header_map {
            let value = value.to_str().map_err(|e| {
                TelemetryError::Init(format!(
                    "gcp credential header `{name}` is not printable ascii: {e}"
                ))
            })?;
            headers.push((name.as_str().to_string(), value.to_string()));
        }
        headers.push((USER_PROJECT_HEADER.to_string(), self.user_project.clone()));
        Ok(headers)
    }
}

/// Per-request header injection for the tonic OTLP exporter.
///
/// The exporter's interceptor hook is synchronous, so headers are read from a
/// shared slot that a background task refreshes via [`GcpHeaderSupplier`].
#[derive(Clone)]
struct AuthHeaderInterceptor {
    headers: Arc<RwLock<Vec<(AsciiMetadataKey, AsciiMetadataValue)>>>,
}

impl AuthHeaderInterceptor {
    fn new(headers: Vec<(AsciiMetadataKey, AsciiMetadataValue)>) -> Self {
        Self { headers: Arc::new(RwLock::new(headers)) }
    }

    /// Spawns the token-refresh loop keeping the header slot current.
    fn spawn_refresh(&self, supplier: GcpHeaderSupplier) {
        let slot = self.headers.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(HEADER_REFRESH_INTERVAL);
            interval.tick().await; // consume the immediate first tick
            loop {
                interval.tick().await;
                match supplier.headers().await {
                    Ok(headers) => match to_metadata_pairs(&headers) {
                        Ok(pairs) => {
                            *slot.write().unwrap_or_else(|p| p.into_inner()) = pairs;
                            tracing::debug!("gcp otlp auth headers refreshed");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "gcp otlp auth header conversion failed");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "gcp otlp auth header refresh failed");
                    }
                }
            }
        });
    }
}

impl tonic::service::Interceptor for AuthHeaderInterceptor {
    fn call(
        &mut self,
        mut request: tonic::Request<()>,
    ) -> Result<tonic::Request<()>, tonic::Status> {
        let headers = self.headers.read().unwrap_or_else(|p| p.into_inner());
        for (key, value) in headers.iter() {
            request.metadata_mut().insert(key.clone(), value.clone());
        }
        Ok(request)
    }
}

/// Converts string header pairs into pre-parsed tonic metadata so the
/// per-request interceptor never parses on the hot path.
fn to_metadata_pairs(
    headers: &[(String, String)],
) -> Result<Vec<(AsciiMetadataKey, AsciiMetadataValue)>, TelemetryError> {
    headers
        .iter()
        .map(|(name, value)| {
            let key = name.parse::<AsciiMetadataKey>().map_err(|e| {
                TelemetryError::Init(format!("invalid gcp telemetry header name `{name}`: {e}"))
            })?;
            let value = value.parse::<AsciiMetadataValue>().map_err(|e| {
                TelemetryError::Init(format!(
                    "invalid gcp telemetry header value for `{name}`: {e}"
                ))
            })?;
            Ok((key, value))
        })
        .collect()
}

/// Exporter hook injecting TLS roots and the auth interceptor.
struct GcpExporterHook {
    interceptor: AuthHeaderInterceptor,
}

impl otlp_pipeline::ExporterHook for GcpExporterHook {
    fn configure<B: opentelemetry_otlp::WithTonicConfig>(&self, builder: B) -> B {
        builder
            .with_tls_config(tonic::transport::ClientTlsConfig::new().with_enabled_roots())
            .with_interceptor(self.interceptor.clone())
    }
}

/// Detects GCP resource attributes from the environment.
///
/// | Attribute | Source |
/// |-----------|--------|
/// | `service.name` | `K_SERVICE`, else `GOOGLE_CLOUD_AGENT_ENGINE_ID`, else `fallback_service_name` |
/// | `gcp.project_id` | `GOOGLE_CLOUD_PROJECT` (omitted when unset) |
/// | `cloud.platform` | `gcp.agent_engine` when `GOOGLE_CLOUD_AGENT_ENGINE_ID` is set |
///
/// `GOOGLE_CLOUD_AGENT_ENGINE_ID` is the bare numeric engine ID that Vertex AI
/// Agent Engine sets in deployed containers.
///
/// # Example
/// ```
/// use adk_telemetry::gcp_resource_attributes;
///
/// let attributes = gcp_resource_attributes("my-agent");
/// assert!(attributes.iter().any(|kv| kv.key.as_str() == "service.name"));
/// ```
pub fn gcp_resource_attributes(fallback_service_name: &str) -> Vec<KeyValue> {
    let non_empty = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let k_service = non_empty(ENV_K_SERVICE);
    let engine_id = non_empty(ENV_AGENT_ENGINE_ID);
    let project_id = non_empty(ENV_PROJECT);

    let service_name = k_service
        .or_else(|| engine_id.clone())
        .unwrap_or_else(|| fallback_service_name.to_string());

    let mut attributes = vec![KeyValue::new("service.name", service_name)];
    if let Some(project_id) = project_id {
        attributes.push(KeyValue::new("gcp.project_id", project_id));
    }
    if engine_id.is_some() {
        // `gcp.agent_engine` is the canonical OpenTelemetry semconv value for
        // Vertex AI Agent Engine, added upstream in
        // open-telemetry/semantic-conventions#2957 (Oct 2025).
        attributes.push(KeyValue::new("cloud.platform", "gcp.agent_engine"));
    }
    attributes
}

/// Initialize telemetry with OTLP trace export to Google Cloud.
///
/// Exports spans to [`GCP_OTLP_ENDPOINT`] with `Authorization: Bearer` headers
/// minted from Application Default Credentials and an `x-goog-user-project`
/// header, and writes Cloud Logging structured JSON to stdout (see
/// [`init_json_logging`] for the log format). Resource attributes come from
/// [`gcp_resource_attributes`].
///
/// Headers are injected per request through a tonic interceptor; a background
/// task re-mints them every five minutes, so exports keep authenticating
/// across token expiry in long-running processes.
///
/// Requires `GOOGLE_CLOUD_PROJECT` to name the project that receives the
/// telemetry. Metrics are not exported on this path — route them through a
/// collector sidecar with [`init_with_otlp`](crate::init_with_otlp) instead.
///
/// # Errors
/// Returns [`TelemetryError::Init`] when `GOOGLE_CLOUD_PROJECT` is unset, ADC
/// is unavailable, the initial token mint fails, or the exporter cannot be
/// built.
///
/// # Example
/// ```no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), adk_telemetry::TelemetryError> {
/// adk_telemetry::init_with_gcp("my-agent").await?;
/// // ... run the agent ...
/// adk_telemetry::shutdown_telemetry();
/// # Ok(())
/// # }
/// ```
pub async fn init_with_gcp(service_name: &str) -> Result<(), TelemetryError> {
    let project_id =
        std::env::var(ENV_PROJECT).ok().filter(|v| !v.is_empty()).ok_or_else(|| {
            TelemetryError::Init(format!(
                "{ENV_PROJECT} must be set to the project that receives telemetry"
            ))
        })?;

    let supplier = GcpHeaderSupplier::new_with_adc(&project_id)?;
    // Fail fast at init when no token can be minted at all.
    let initial = supplier.headers().await?;
    let interceptor = AuthHeaderInterceptor::new(to_metadata_pairs(&initial)?);
    interceptor.spawn_refresh(supplier);

    let resource = opentelemetry_sdk::Resource::builder_empty()
        .with_attributes(gcp_resource_attributes(service_name))
        .build();
    let tracer =
        otlp_pipeline::build_tracer(resource, GCP_OTLP_ENDPOINT, &GcpExporterHook { interceptor })?;

    let service_name = service_name.to_string();
    INIT.call_once(move || {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap_or_else(|_| EnvFilter::new("info"));

        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .event_format(CloudLoggingJsonFormat::new(Some(project_id)))
                    .with_filter(filter),
            )
            .with(tracing_opentelemetry::OpenTelemetryLayer::new(tracer))
            .init();

        tracing::info!(
            service.name = service_name,
            otlp.endpoint = GCP_OTLP_ENDPOINT,
            "telemetry initialized with google cloud otlp export"
        );
    });

    Ok(())
}

/// Initialize structured JSON logging for Cloud Logging.
///
/// Writes one JSON object per line to stdout using
/// [`CloudLoggingJsonFormat`], so Cloud Logging parses severity, message, and
/// trace correlation fields instead of showing opaque text payloads. The trace
/// resource name uses `GOOGLE_CLOUD_PROJECT` when set.
///
/// Trace correlation fields are emitted when an OpenTelemetry layer is active
/// in the subscriber — [`init_with_gcp`] installs both; this function installs
/// logging only.
///
/// # Errors
/// Currently infallible; returns `Result` for signature stability with the
/// other `init_*` functions.
///
/// # Example
/// ```
/// use adk_telemetry::init_json_logging;
///
/// init_json_logging().expect("failed to initialize json logging");
/// tracing::info!(user.id = "u1", "request handled");
/// ```
pub fn init_json_logging() -> Result<(), TelemetryError> {
    let project_id = std::env::var(ENV_PROJECT).ok().filter(|v| !v.is_empty());
    INIT.call_once(move || {
        let filter = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap_or_else(|_| EnvFilter::new("info"));

        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .event_format(CloudLoggingJsonFormat::new(project_id)),
            )
            .init();

        tracing::info!("telemetry initialized with cloud logging json output");
    });
    Ok(())
}

/// Maps a tracing level to its Cloud Logging severity name.
///
/// Cloud Logging has no `TRACE` severity and spells warnings `WARNING`;
/// everything else matches the tracing name.
fn cloud_severity(level: tracing::Level) -> &'static str {
    // `tracing::Level` is not pattern-matchable (associated constants on an
    // opaque struct), so this is an equality chain rather than a `match`.
    if level == tracing::Level::ERROR {
        "ERROR"
    } else if level == tracing::Level::WARN {
        "WARNING"
    } else if level == tracing::Level::INFO {
        "INFO"
    } else {
        "DEBUG"
    }
}

/// Formats the Cloud Logging trace resource name for a trace ID.
fn cloud_trace_resource(project_id: &str, trace_id: &opentelemetry::trace::TraceId) -> String {
    format!("projects/{project_id}/traces/{trace_id}")
}

/// Event formatter emitting Cloud Logging structured JSON.
///
/// One JSON object per line with:
///
/// | Field | Content |
/// |-------|---------|
/// | `timestamp` | RFC 3339 event time |
/// | `severity` | Cloud Logging severity mapped from the tracing level |
/// | `message` | The event message |
/// | `target` | The tracing target |
/// | `logging.googleapis.com/trace` | `projects/{project}/traces/{trace_id}` from the active OpenTelemetry span |
/// | `logging.googleapis.com/spanId` | Span ID from the active OpenTelemetry span |
/// | `logging.googleapis.com/trace_sampled` | Sampling decision |
/// | *(event fields)* | Each field as a typed JSON value |
/// | *(span fields)* | Fields of in-scope spans (innermost wins, never overrides event fields) |
///
/// Trace fields are only present when a `tracing-opentelemetry` layer is
/// registered in the same subscriber and the event fires inside a span.
///
/// # Example
/// ```
/// use adk_telemetry::CloudLoggingJsonFormat;
/// use tracing_subscriber::layer::SubscriberExt;
///
/// let subscriber = tracing_subscriber::registry().with(
///     tracing_subscriber::fmt::layer()
///         .json()
///         .event_format(CloudLoggingJsonFormat::new(Some("my-project".to_string()))),
/// );
/// ```
pub struct CloudLoggingJsonFormat {
    project_id: Option<String>,
}

impl CloudLoggingJsonFormat {
    /// Creates a formatter. `project_id` is required for the
    /// `logging.googleapis.com/trace` resource name; without it, only the
    /// span ID and sampling fields are emitted.
    pub fn new(project_id: Option<String>) -> Self {
        Self { project_id }
    }
}

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for CloudLoggingJsonFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        use tracing_subscriber::fmt::time::FormatTime;

        let mut fields = serde_json::Map::new();

        let mut timestamp = String::new();
        if tracing_subscriber::fmt::time::SystemTime
            .format_time(&mut tracing_subscriber::fmt::format::Writer::new(&mut timestamp))
            .is_ok()
        {
            fields.insert("timestamp".to_string(), serde_json::Value::String(timestamp));
        }

        let metadata = event.metadata();
        fields.insert(
            "severity".to_string(),
            serde_json::Value::String(cloud_severity(*metadata.level()).to_string()),
        );
        fields
            .insert("target".to_string(), serde_json::Value::String(metadata.target().to_string()));

        let mut visitor = JsonFieldVisitor::default();
        event.record(&mut visitor);
        if let Some(message) = visitor.message {
            fields.insert("message".to_string(), message);
        }
        for (name, value) in visitor.fields {
            fields.insert(name, value);
        }

        // Trace correlation from the active OpenTelemetry span, when a
        // tracing-opentelemetry layer is registered in this subscriber.
        let otel_context =
            tracing_opentelemetry::OpenTelemetrySpanExt::context(&tracing::Span::current());
        let otel_span = opentelemetry::trace::TraceContextExt::span(&otel_context);
        let span_context = otel_span.span_context();
        if span_context.is_valid() {
            if let Some(project_id) = &self.project_id {
                fields.insert(
                    LOG_TRACE_KEY.to_string(),
                    serde_json::Value::String(cloud_trace_resource(
                        project_id,
                        &span_context.trace_id(),
                    )),
                );
            }
            fields.insert(
                LOG_SPAN_ID_KEY.to_string(),
                serde_json::Value::String(span_context.span_id().to_string()),
            );
            fields.insert(
                LOG_TRACE_SAMPLED_KEY.to_string(),
                serde_json::Value::Bool(span_context.is_sampled()),
            );
        }

        // Span fields recorded by the fmt layer's JSON field formatter.
        // Innermost span first; existing keys (event fields) are never overridden.
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                let extensions = span.extensions();
                let Some(formatted) =
                    extensions.get::<tracing_subscriber::fmt::FormattedFields<N>>()
                else {
                    continue;
                };
                let Ok(span_fields) = serde_json::from_str::<
                    serde_json::Map<String, serde_json::Value>,
                >(&formatted.fields) else {
                    continue;
                };
                for (name, value) in span_fields {
                    fields.entry(name).or_insert(value);
                }
            }
        }

        writeln!(writer, "{}", serde_json::Value::Object(fields))
    }
}

/// Collects event fields into typed JSON values, separating `message`.
#[derive(Default)]
struct JsonFieldVisitor {
    message: Option<serde_json::Value>,
    fields: Vec<(String, serde_json::Value)>,
}

impl JsonFieldVisitor {
    fn record(&mut self, field: &tracing::field::Field, value: serde_json::Value) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl tracing::field::Visit for JsonFieldVisitor {
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.record(field, serde_json::json!(value));
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record(field, serde_json::json!(value));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.record(field, serde_json::json!(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.record(field, serde_json::json!(value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record(field, serde_json::Value::String(value.to_string()));
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.record(field, serde_json::Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.record(field, serde_json::Value::String(format!("{value:?}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use google_cloud_auth::credentials::{CredentialsProvider, EntityTag};
    use std::sync::Mutex;

    /// Serializes env-var mutation across tests. `cargo nextest` isolates each
    /// test in its own process, but plain `cargo test` shares one.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let saved: Vec<(&str, Option<String>)> =
            vars.iter().map(|(name, _)| (*name, std::env::var(name).ok())).collect();
        // SAFETY: mutation is serialized by ENV_LOCK and scoped to this helper.
        for (name, value) in vars {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        f();
        for (name, value) in saved {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }

    #[derive(Debug)]
    struct FakeCredentials;

    impl CredentialsProvider for FakeCredentials {
        fn headers(
            &self,
            _extensions: http::Extensions,
        ) -> impl std::future::Future<
            Output = std::result::Result<
                CacheableResource<http::HeaderMap>,
                google_cloud_auth::errors::CredentialsError,
            >,
        > + Send {
            let mut data = http::HeaderMap::new();
            data.insert(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_static("Bearer fake-token"),
            );
            std::future::ready(Ok(CacheableResource::New { entity_tag: EntityTag::new(), data }))
        }

        fn universe_domain(&self) -> impl std::future::Future<Output = Option<String>> + Send {
            std::future::ready(None)
        }
    }

    #[tokio::test]
    async fn header_supplier_mints_authorization_and_user_project() {
        let supplier =
            GcpHeaderSupplier::with_credentials(Credentials::from(FakeCredentials), "test-project");
        let headers = supplier.headers().await.expect("mint headers");
        assert_eq!(
            headers,
            vec![
                ("authorization".to_string(), "Bearer fake-token".to_string()),
                ("x-goog-user-project".to_string(), "test-project".to_string()),
            ]
        );
    }

    #[test]
    fn interceptor_injects_headers_per_request() {
        use tonic::service::Interceptor;

        let pairs = to_metadata_pairs(&[
            ("authorization".to_string(), "Bearer fake-token".to_string()),
            ("x-goog-user-project".to_string(), "test-project".to_string()),
        ])
        .expect("convert headers");
        let mut interceptor = AuthHeaderInterceptor::new(pairs);

        let request = interceptor.call(tonic::Request::new(())).expect("intercept");
        let metadata = request.metadata();
        assert_eq!(
            metadata.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer fake-token")
        );
        assert_eq!(
            metadata.get("x-goog-user-project").and_then(|v| v.to_str().ok()),
            Some("test-project")
        );
    }

    #[test]
    fn metadata_conversion_rejects_invalid_header_names() {
        let result = to_metadata_pairs(&[("not a header".to_string(), "value".to_string())]);
        assert!(result.is_err());
    }

    #[test]
    fn resource_attributes_prefer_k_service_and_detect_agent_engine() {
        with_env(
            &[
                (ENV_PROJECT, Some("my-project")),
                (ENV_K_SERVICE, Some("my-run-service")),
                (ENV_AGENT_ENGINE_ID, Some("1234567890")),
            ],
            || {
                assert_eq!(
                    gcp_resource_attributes("fallback"),
                    vec![
                        KeyValue::new("service.name", "my-run-service"),
                        KeyValue::new("gcp.project_id", "my-project"),
                        KeyValue::new("cloud.platform", "gcp.agent_engine"),
                    ]
                );
            },
        );
    }

    #[test]
    fn resource_attributes_fall_back_to_engine_id_then_argument() {
        with_env(
            &[
                (ENV_PROJECT, None),
                (ENV_K_SERVICE, None),
                (ENV_AGENT_ENGINE_ID, Some("1234567890")),
            ],
            || {
                assert_eq!(
                    gcp_resource_attributes("fallback"),
                    vec![
                        KeyValue::new("service.name", "1234567890"),
                        KeyValue::new("cloud.platform", "gcp.agent_engine"),
                    ]
                );
            },
        );
        with_env(
            &[(ENV_PROJECT, None), (ENV_K_SERVICE, None), (ENV_AGENT_ENGINE_ID, None)],
            || {
                assert_eq!(
                    gcp_resource_attributes("fallback"),
                    vec![KeyValue::new("service.name", "fallback")]
                );
            },
        );
    }

    #[test]
    fn severity_mapping_matches_cloud_logging_names() {
        assert_eq!(cloud_severity(tracing::Level::TRACE), "DEBUG");
        assert_eq!(cloud_severity(tracing::Level::DEBUG), "DEBUG");
        assert_eq!(cloud_severity(tracing::Level::INFO), "INFO");
        assert_eq!(cloud_severity(tracing::Level::WARN), "WARNING");
        assert_eq!(cloud_severity(tracing::Level::ERROR), "ERROR");
    }

    #[test]
    fn trace_resource_name_uses_project_and_trace_id() {
        let trace_id = opentelemetry::trace::TraceId::from_hex("0123456789abcdef0123456789abcdef")
            .expect("valid trace id");
        assert_eq!(
            cloud_trace_resource("my-project", &trace_id),
            "projects/my-project/traces/0123456789abcdef0123456789abcdef"
        );
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap_or_else(|p| p.into_inner()).extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
        type Writer = SharedBuf;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn json_format_emits_cloud_logging_fields() {
        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .event_format(CloudLoggingJsonFormat::new(Some("my-project".to_string())))
                .with_writer(buf.clone()),
        );

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request", request.id = "r1");
            let _enter = span.enter();
            tracing::warn!(status.code = 429, "rate limited");
        });

        let output = String::from_utf8(buf.0.lock().unwrap_or_else(|p| p.into_inner()).clone())
            .expect("utf8 output");
        let line = output.lines().next().expect("one log line");
        let value: serde_json::Value = serde_json::from_str(line).expect("valid json");

        assert_eq!(value["severity"], "WARNING");
        assert_eq!(value["message"], "rate limited");
        assert_eq!(value["status.code"], 429);
        assert_eq!(value["request.id"], "r1");
        assert!(value["timestamp"].is_string());
        // No OpenTelemetry layer registered, so no trace correlation fields.
        assert!(value.get(LOG_TRACE_KEY).is_none());
    }
}
