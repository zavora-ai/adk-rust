use std::collections::HashMap;
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use tracing::{Id, Subscriber, debug};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// Destination for spans captured by [`AdkSpanLayer`].
///
/// Implemented by [`AdkSpanExporter`] (in-memory, queried by the server debug
/// routes) and, with the `sqlite` feature, by
/// `SqliteSpanExporter` (persistent,
/// zero-infrastructure tracing). Implementations decide which spans to keep —
/// the layer forwards every closed span.
pub trait SpanSink: Send + Sync {
    /// Receive one closed span with its collected attributes.
    fn export_span(&self, span_name: &str, attributes: HashMap<String, String>);
}

/// ADK-Go style span exporter that retains runtime spans in memory.
///
/// Spans are keyed by a stable span ID while preserving the originating ADK
/// event ID as an attribute. This lets multiple runtime operations describe the
/// same event without overwriting one another.
#[derive(Debug, Clone, Default)]
pub struct AdkSpanExporter {
    /// Map of span ID to span attributes.
    trace_dict: Arc<RwLock<HashMap<String, HashMap<String, String>>>>,
    /// Whether this exporter has observed at least one retained runtime span.
    collecting: Arc<AtomicBool>,
}

impl AdkSpanExporter {
    /// Creates an empty in-process span exporter.
    pub fn new() -> Self {
        Self {
            trace_dict: Arc::new(RwLock::new(HashMap::new())),
            collecting: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns a snapshot of retained spans keyed by span ID.
    pub fn get_trace_dict(&self) -> HashMap<String, HashMap<String, String>> {
        self.trace_dict.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Returns the first span associated with an ADK event ID.
    pub fn get_trace_by_event_id(&self, event_id: &str) -> Option<HashMap<String, String>> {
        debug!("AdkSpanExporter::get_trace_by_event_id called with event_id: {}", event_id);
        let trace_dict = self.trace_dict.read().unwrap_or_else(|e| e.into_inner());
        let result = trace_dict.get(event_id).cloned().or_else(|| {
            trace_dict
                .values()
                .find(|attributes| {
                    attributes.get("gcp.vertex.agent.event_id").is_some_and(|id| id == event_id)
                })
                .cloned()
        });
        debug!("get_trace_by_event_id result for event_id '{}': {:?}", event_id, result.is_some());
        result
    }

    /// Returns whether the exporter has retained at least one runtime span.
    ///
    /// A configured exporter reports `false` until a supported span closes.
    /// Servers use this to distinguish a ready collector from one proven to be
    /// collecting, instead of advertising telemetry from configuration alone.
    pub fn is_collecting(&self) -> bool {
        self.collecting.load(Ordering::Acquire)
    }

    /// Get all spans for a session (by filtering spans that have matching session_id)
    pub fn get_session_trace(&self, session_id: &str) -> Vec<HashMap<String, String>> {
        debug!("AdkSpanExporter::get_session_trace called with session_id: {}", session_id);
        let trace_dict = self.trace_dict.read().unwrap_or_else(|e| e.into_inner());

        let mut spans = Vec::new();
        for attributes in trace_dict.values() {
            // Check if this span belongs to the session
            if let Some(span_session_id) = attributes.get("gcp.vertex.agent.session_id")
                && span_session_id == session_id
            {
                spans.push(attributes.clone());
            }
        }

        debug!("get_session_trace result for session_id '{}': {} spans", session_id, spans.len());
        spans
    }
}

impl SpanSink for AdkSpanExporter {
    /// Stores supported runtime spans in memory for the debug API.
    fn export_span(&self, span_name: &str, attributes: HashMap<String, String>) {
        if is_runtime_span(span_name) {
            if let Some(event_id) = attributes.get("gcp.vertex.agent.event_id") {
                debug!(
                    "AdkSpanExporter: Storing span '{}' with event_id '{}'",
                    span_name, event_id
                );
                let storage_key =
                    attributes.get("span_id").cloned().unwrap_or_else(|| event_id.clone());
                let mut trace_dict = self.trace_dict.write().unwrap_or_else(|e| e.into_inner());
                trace_dict.insert(storage_key, attributes);
                self.collecting.store(true, Ordering::Release);
                debug!("AdkSpanExporter: Span stored, total spans: {}", trace_dict.len());
            } else {
                debug!("AdkSpanExporter: Skipping span '{}' - no event_id found", span_name);
            }
        } else {
            debug!("AdkSpanExporter: Skipping span '{}' - not in allowed list", span_name);
        }
    }
}

pub(crate) fn is_runtime_span(span_name: &str) -> bool {
    span_name == "agent.execute"
        || span_name == "call_llm"
        || span_name == "send_data"
        || span_name.starts_with("execute_tool")
        || matches!(span_name, "team.run" | "team.member.run" | "team.relationship.execute")
}

/// Tracing layer that captures spans and exports them via a [`SpanSink`]
/// (in-memory [`AdkSpanExporter`], SQLite, or any custom sink).
pub struct AdkSpanLayer {
    exporter: Arc<dyn SpanSink>,
}

impl AdkSpanLayer {
    pub fn new<S: SpanSink + 'static>(exporter: Arc<S>) -> Self {
        Self { exporter }
    }
}

#[derive(Clone)]
struct SpanFields {
    values: HashMap<String, String>,
    event_id_declared: bool,
}

#[derive(Clone)]
struct SpanTiming {
    start_time: std::time::Instant,
}

impl<S> Layer<S> for AdkSpanLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();

        // Record start time
        extensions.insert(SpanTiming { start_time: std::time::Instant::now() });

        // Capture fields
        let mut visitor = StringVisitor::default();
        attrs.record(&mut visitor);
        let mut fields_map = visitor.0;
        let event_id_declared = fields_map.contains_key("gcp.vertex.agent.event_id");

        // Propagate fields from parent span (for context inheritance)
        if let Some(parent) = span.parent()
            && let Some(parent_fields) = parent.extensions().get::<SpanFields>()
        {
            let context_keys = [
                "gcp.vertex.agent.session_id",
                "gcp.vertex.agent.invocation_id",
                "gcp.vertex.agent.event_id",
                "gen_ai.conversation.id",
                #[cfg(feature = "genai-semconv")]
                "gen_ai.provider.name",
                #[cfg(feature = "genai-semconv")]
                "gen_ai.system",
            ];

            for key in context_keys {
                if !fields_map.contains_key(key)
                    && let Some(val) = parent_fields.values.get(key)
                {
                    fields_map.insert(key.to_string(), val.clone());
                }
            }
        }

        extensions.insert(SpanFields { values: fields_map, event_id_declared });
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<SpanFields>() {
            let mut visitor = StringVisitor::default();
            values.record(&mut visitor);
            for (k, v) in visitor.0 {
                if k == "gcp.vertex.agent.event_id" {
                    fields.event_id_declared = true;
                }
                fields.values.insert(k, v);
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();

        // Calculate actual duration
        let timing = extensions.get::<SpanTiming>();
        let end_time = std::time::Instant::now();
        let duration_nanos =
            timing.map(|t| end_time.duration_since(t.start_time).as_nanos() as u64).unwrap_or(0);

        // Get captured fields
        let span_fields = extensions.get::<SpanFields>();
        let event_id_declared = span_fields.is_some_and(|fields| fields.event_id_declared);
        let mut attributes = span_fields.map(|fields| fields.values.clone()).unwrap_or_default();

        // Get span name - prefer otel.name attribute (for dynamic names), fallback to metadata
        let metadata = span.metadata();
        let span_name =
            attributes.get("otel.name").cloned().unwrap_or_else(|| metadata.name().to_string());

        // Add span metadata and actual timing with unique IDs
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        // Use invocation_id as trace_id (for grouping in UI). Spans that
        // declare their own event ID keep it as the span ID for compatibility;
        // child spans that inherit a parent event ID use tracing's unique ID so
        // they cannot overwrite the parent or a sibling. `send_data` describes
        // the same event as its enclosing `call_llm`, so it also needs its own
        // ID to preserve both operations.
        let generated_span_id = format!("{:016x}", id.into_u64());
        let invocation_id = attributes
            .get("gcp.vertex.agent.invocation_id")
            .cloned()
            .unwrap_or_else(|| generated_span_id.clone());
        let event_id = attributes
            .get("gcp.vertex.agent.event_id")
            .cloned()
            .unwrap_or_else(|| generated_span_id.clone());
        let span_id = if event_id_declared && span_name != "send_data" {
            event_id
        } else {
            generated_span_id
        };

        attributes.insert("span_name".to_string(), span_name.clone());
        attributes.insert("trace_id".to_string(), invocation_id); // Group by invocation
        attributes.insert("span_id".to_string(), span_id);
        attributes.insert("start_time".to_string(), (now_nanos - duration_nanos).to_string());
        attributes.insert("end_time".to_string(), now_nanos.to_string());

        // Don't set parent_span_id to keep all spans at same level like ADK-Go

        // Export the span
        self.exporter.export_span(&span_name, attributes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing_subscriber::{
        EnvFilter,
        filter::filter_fn,
        layer::{Layer, SubscriberExt},
    };

    #[test]
    fn test_conversation_id_propagates_to_child_spans() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let layer = AdkSpanLayer::new(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let parent = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.event_id" = "evt-parent",
                "gcp.vertex.agent.invocation_id" = "inv-1",
                "gcp.vertex.agent.session_id" = "session-1",
                "gen_ai.conversation.id" = "session-1",
                "agent.name" = "test-agent"
            );

            let _parent_guard = parent.enter();

            let child = tracing::info_span!(
                "call_llm",
                "gcp.vertex.agent.event_id" = "evt-child",
                "gcp.vertex.agent.llm_request" = "{}"
            );
            let _child_guard = child.enter();
            tracing::info!("child span body");
        });

        let child_trace =
            exporter.get_trace_by_event_id("evt-child").expect("child span should be exported");
        assert_eq!(
            child_trace.get("gen_ai.conversation.id").map(String::as_str),
            Some("session-1")
        );
    }

    #[test]
    fn console_log_filter_does_not_suppress_runtime_span_capture() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let capture = AdkSpanLayer::new(exporter.clone()).with_filter(filter_fn(|metadata| {
            metadata.is_span() && is_runtime_span(metadata.name())
        }));
        let console = tracing_subscriber::fmt::layer()
            .with_writer(std::io::sink)
            .with_filter(EnvFilter::new("warn"));
        let subscriber = tracing_subscriber::registry().with(console).with(capture);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.event_id" = "evt-filtered-console",
                "gcp.vertex.agent.invocation_id" = "inv-filtered-console",
                "gcp.vertex.agent.session_id" = "session-filtered-console"
            );
            let _guard = span.enter();
        });

        assert!(exporter.get_trace_by_event_id("evt-filtered-console").is_some());
        assert!(exporter.is_collecting());
    }

    #[test]
    fn inherited_event_ids_do_not_overwrite_team_relationship_spans() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let layer = AdkSpanLayer::new(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let parent = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.event_id" = "evt-team",
                "gcp.vertex.agent.invocation_id" = "inv-team",
                "gcp.vertex.agent.session_id" = "session-team"
            );
            let parent_guard = parent.enter();
            let relationship = tracing::info_span!(
                "team.relationship.execute",
                team.name = "support",
                team.relationship.from = "supervisor",
                team.relationship.to = "billing",
                team.relationship.kind = "handoff",
                team.edge.id = "edge-1"
            );
            let relationship_guard = relationship.enter();
            drop(relationship_guard);
            drop(relationship);
            drop(parent_guard);
            drop(parent);
        });

        let spans = exporter.get_session_trace("session-team");
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().any(|span| {
            span.get("span_name").is_some_and(|name| name == "team.relationship.execute")
        }));
        let unique_span_ids = spans
            .iter()
            .filter_map(|span| span.get("span_id"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique_span_ids.len(), 2);
    }

    #[test]
    fn send_data_does_not_overwrite_call_llm_for_the_same_event() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let layer = AdkSpanLayer::new(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let call_llm = tracing::info_span!(
                "call_llm",
                "gcp.vertex.agent.event_id" = "evt-model",
                "gcp.vertex.agent.invocation_id" = "inv-model",
                "gcp.vertex.agent.session_id" = "session-model"
            );
            drop(call_llm);

            let send_data = tracing::info_span!(
                "send_data",
                "gcp.vertex.agent.event_id" = "evt-model",
                "gcp.vertex.agent.invocation_id" = "inv-model",
                "gcp.vertex.agent.session_id" = "session-model"
            );
            drop(send_data);
        });

        let spans = exporter.get_session_trace("session-model");
        assert_eq!(spans.len(), 2);
        assert!(
            spans.iter().any(|span| span.get("span_name").is_some_and(|name| name == "call_llm"))
        );
        assert!(
            spans.iter().any(|span| span.get("span_name").is_some_and(|name| name == "send_data"))
        );
    }
}

#[derive(Default)]
struct StringVisitor(HashMap<String, String>);

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{:?}", value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}
