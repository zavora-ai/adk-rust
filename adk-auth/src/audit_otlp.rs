//! OpenTelemetry OTLP audit sink for enterprise audit logging.
//!
//! Exports audit events as OpenTelemetry log records to any OTLP-compatible
//! collector (Grafana Loki, Datadog, Splunk, Elastic, etc.). Requires the
//! `otlp-audit` feature.
//!
//! Each audit event is emitted as a structured `tracing` event at the appropriate
//! level, which is then exported via the OpenTelemetry tracing-opentelemetry bridge.
//! This approach integrates with the existing `adk-telemetry` OTLP pipeline.
//!
//! Alternatively, events are exported directly via the OTel Logs SDK when
//! `adk-telemetry` is not in use.
//!
//! # Architecture
//!
//! The sink uses the OpenTelemetry Logs SDK directly:
//! 1. Creates a `SdkLoggerProvider` with a batch OTLP exporter
//! 2. Each audit event becomes a log record with structured attributes
//! 3. Severity is mapped from the audit outcome
//! 4. The provider is flushed on `flush()` and shut down on `Drop`
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_auth::audit_otlp::OtlpAuditSink;
//!
//! // Uses OTEL_EXPORTER_OTLP_ENDPOINT env var (default: http://localhost:4317)
//! let sink = OtlpAuditSink::new()?;
//! sink.log(AuditEvent::tool_access("alice", "search", AuditOutcome::Allowed)).await?;
//! sink.flush().await?; // Ensure events are exported
//! ```

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use tracing::debug;

use crate::AuthError;
use crate::audit::{AuditEvent, AuditOutcome, AuditSink};

/// OpenTelemetry OTLP audit sink.
///
/// Exports audit events as structured log records to an OTLP collector.
/// Configure the endpoint via `OTEL_EXPORTER_OTLP_ENDPOINT` environment
/// variable (default: `http://localhost:4317`).
pub struct OtlpAuditSink {
    provider: SdkLoggerProvider,
}

impl OtlpAuditSink {
    /// Create a new OTLP audit sink with default configuration.
    ///
    /// Reads `OTEL_EXPORTER_OTLP_ENDPOINT` for the collector address.
    /// Falls back to `http://localhost:4317` if not set.
    pub fn new() -> Result<Self, AuthError> {
        use opentelemetry_otlp::LogExporter;

        let exporter = LogExporter::builder()
            .with_tonic()
            .build()
            .map_err(|e| AuthError::AuditError(format!("OTLP log exporter build failed: {e}")))?;

        let provider = SdkLoggerProvider::builder().with_batch_exporter(exporter).build();

        Ok(Self { provider })
    }

    /// Map audit outcome to OTel severity level.
    fn severity_from_outcome(outcome: &AuditOutcome) -> Severity {
        match outcome {
            AuditOutcome::Allowed | AuditOutcome::Created | AuditOutcome::Updated => Severity::Info,
            AuditOutcome::Denied | AuditOutcome::Blocked | AuditOutcome::Paused => Severity::Warn,
            AuditOutcome::Error => Severity::Error,
            AuditOutcome::Deleted | AuditOutcome::Escalated => Severity::Warn,
        }
    }
}

#[async_trait::async_trait]
impl AuditSink for OtlpAuditSink {
    async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
        let logger = self.provider.logger("adk-auth-audit");

        let severity = Self::severity_from_outcome(&event.outcome);
        let body = serde_json::to_string(&event)
            .map_err(|e| AuthError::AuditError(format!("serialize event: {e}")))?;

        let mut attributes: Vec<(&str, String)> = vec![
            ("audit.user", event.user.clone()),
            ("audit.resource", event.resource.clone()),
            (
                "audit.event_type",
                serde_json::to_value(&event.event_type)
                    .unwrap_or_default()
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
            ),
            (
                "audit.outcome",
                serde_json::to_value(&event.outcome)
                    .unwrap_or_default()
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
            ),
        ];

        if let Some(ref ws) = event.workspace_id {
            attributes.push(("audit.workspace_id", ws.clone()));
        }
        if let Some(ref tid) = event.tenant_id {
            attributes.push(("audit.tenant_id", tid.clone()));
        }
        if let Some(ref rid) = event.request_id {
            attributes.push(("audit.request_id", rid.clone()));
        }
        if let Some(ref ip) = event.ip_address {
            attributes.push(("audit.ip_address", ip.clone()));
        }
        if let Some(ref res_id) = event.resource_id {
            attributes.push(("audit.resource_id", res_id.clone()));
        }
        if let Some(ref action) = event.action {
            attributes.push(("audit.action", action.clone()));
        }
        if let Some(ref session) = event.session_id {
            attributes.push(("audit.session_id", session.clone()));
        }
        if let Some(ref agent) = event.agent_id {
            attributes.push(("audit.agent_id", agent.clone()));
        }
        if let Some(ref subject) = event.on_behalf_of {
            attributes
                .push(("audit.on_behalf_of", serde_json::to_string(subject).unwrap_or_default()));
        }

        let mut record = logger.create_log_record();
        record.set_severity_number(severity);
        record.set_body(body.into());

        for (key, value) in attributes {
            record.add_attribute(key, AnyValue::String(value.into()));
        }

        logger.emit(record);
        debug!("audit event emitted to OTLP");
        Ok(())
    }

    async fn log_batch(&self, events: Vec<AuditEvent>) -> Result<(), AuthError> {
        for event in events {
            self.log(event).await?;
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), AuthError> {
        self.provider
            .force_flush()
            .map_err(|e| AuthError::AuditError(format!("OTLP flush failed: {e}")))?;
        Ok(())
    }
}

impl Drop for OtlpAuditSink {
    fn drop(&mut self) {
        if let Err(e) = self.provider.shutdown() {
            tracing::warn!("OTLP audit sink shutdown error: {e}");
        }
    }
}
