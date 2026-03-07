use crate::visitor::StringVisitor;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{Id, Subscriber, debug};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// ADK-Go style span exporter that stores spans by event_id
/// Follows the pattern from APIServerSpanExporter in ADK-Go
#[derive(Debug, Clone, Default)]
pub struct AdkSpanExporter {
    /// Map of event_id -> span attributes (following ADK-Go pattern)
    trace_dict: Arc<RwLock<HashMap<String, HashMap<String, String>>>>,
    /// Index of session_id -> list of event_ids for O(1) lookup
    session_index: Arc<RwLock<HashMap<String, Vec<String>>>>,
}

impl AdkSpanExporter {
    pub fn new() -> Self {
        Self {
            trace_dict: Arc::new(RwLock::new(HashMap::new())),
            session_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get trace dict (following ADK-Go GetTraceDict method)
    pub fn get_trace_dict(&self) -> HashMap<String, HashMap<String, String>> {
        self.trace_dict.read().unwrap().clone()
    }

    /// Get trace by event_id (following ADK-Go pattern)
    pub fn get_trace_by_event_id(&self, event_id: &str) -> Option<HashMap<String, String>> {
        debug!("AdkSpanExporter::get_trace_by_event_id called with event_id: {}", event_id);
        let trace_dict = self.trace_dict.read().unwrap();
        let result = trace_dict.get(event_id).cloned();
        debug!("get_trace_by_event_id result for event_id '{}': {:?}", event_id, result.is_some());
        result
    }

    /// Get all spans for a session (by filtering spans that have matching session_id)
    pub fn get_session_trace(&self, session_id: &str) -> Vec<HashMap<String, String>> {
        debug!("AdkSpanExporter::get_session_trace called with session_id: {}", session_id);

        // O(1) lookup using session_index without cloning event_ids
        let session_index = self.session_index.read().unwrap();
        if let Some(event_ids) = session_index.get(session_id) {
            let trace_dict = self.trace_dict.read().unwrap();
            let spans: Vec<_> =
                event_ids.iter().filter_map(|id| trace_dict.get(id).cloned()).collect();
            debug!(
                "get_session_trace result for session_id '{}': {} spans",
                session_id,
                spans.len()
            );
            spans
        } else {
            debug!("get_session_trace result for session_id '{}': 0 spans", session_id);
            Vec::new()
        }
    }

    /// Internal method to store span (following ADK-Go ExportSpans pattern)
    fn export_span(&self, span_name: &str, attributes: HashMap<String, String>) {
        // Only capture specific span names (following ADK-Go pattern)
        if span_name == "agent.execute"
            || span_name == "call_llm"
            || span_name == "send_data"
            || span_name.starts_with("execute_tool")
        {
            let event_id_opt = attributes.get("adk.agent.event_id").cloned();
            let session_id_opt = attributes.get("adk.agent.session_id").cloned();

            if let Some(event_id) = event_id_opt {
                debug!(
                    "AdkSpanExporter: Storing span '{}' with event_id '{}'",
                    span_name, event_id
                );

                // Acquire locks in consistent order: session_index then trace_dict
                // to prevent potential deadlocks with get_session_trace
                let mut session_index = self.session_index.write().unwrap();
                let mut trace_dict = self.trace_dict.write().unwrap();

                if trace_dict.insert(event_id.clone(), attributes).is_none() {
                    debug!("AdkSpanExporter: Span stored");
                    // Update session index if session_id exists and it's a new event
                    if let Some(sid) = session_id_opt {
                        session_index.entry(sid).or_default().push(event_id);
                    }
                } else {
                    debug!("AdkSpanExporter: Span updated");
                }
            } else {
                debug!("AdkSpanExporter: Skipping span '{}' - no event_id found", span_name);
            }
        } else {
            debug!("AdkSpanExporter: Skipping span '{}' - not in allowed list", span_name);
        }
    }
}

/// Tracing layer that captures spans and exports them via AdkSpanExporter
pub struct AdkSpanLayer {
    exporter: Arc<AdkSpanExporter>,
}

impl AdkSpanLayer {
    pub fn new(exporter: Arc<AdkSpanExporter>) -> Self {
        Self { exporter }
    }
}

#[derive(Clone)]
struct SpanFields(HashMap<String, String>);

#[derive(Clone)]
struct SpanTiming {
    start_time: std::time::Instant,
}

impl<S> Layer<S> for AdkSpanLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found");
        let mut extensions = span.extensions_mut();

        // Record start time
        extensions.insert(SpanTiming { start_time: std::time::Instant::now() });

        // Capture fields
        let mut visitor = StringVisitor::default();
        attrs.record(&mut visitor);
        let mut fields_map = visitor.0;

        // Propagate fields from parent span (for context inheritance)
        if let Some(parent) = span.parent() {
            if let Some(parent_fields) = parent.extensions().get::<SpanFields>() {
                let context_keys = [
                    "adk.agent.session_id",
                    "adk.agent.invocation_id",
                    "adk.agent.event_id",
                    "gen_ai.conversation.id",
                ];

                for key in context_keys {
                    if !fields_map.contains_key(key) {
                        if let Some(val) = parent_fields.0.get(key) {
                            fields_map.insert(key.to_string(), val.clone());
                        }
                    }
                }
            }
        }

        extensions.insert(SpanFields(fields_map));
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found");
        let mut extensions = span.extensions_mut();
        if let Some(fields) = extensions.get_mut::<SpanFields>() {
            let mut visitor = StringVisitor::default();
            values.record(&mut visitor);
            for (k, v) in visitor.0 {
                fields.0.insert(k, v);
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found");
        let extensions = span.extensions();

        // Calculate actual duration
        let timing = extensions.get::<SpanTiming>();
        let end_time = std::time::Instant::now();
        let duration_nanos =
            timing.map(|t| end_time.duration_since(t.start_time).as_nanos() as u64).unwrap_or(0);

        // Get captured fields
        let mut attributes =
            extensions.get::<SpanFields>().map(|f| f.0.clone()).unwrap_or_default();

        // Get span name - prefer otel.name attribute (for dynamic names), fallback to metadata
        let metadata = span.metadata();
        let span_name =
            attributes.get("otel.name").cloned().unwrap_or_else(|| metadata.name().to_string());

        // Add span metadata and actual timing with unique IDs
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        // Use invocation_id as trace_id (for grouping in UI)
        // Use event_id as span_id (for uniqueness)
        let invocation_id = attributes
            .get("adk.agent.invocation_id")
            .cloned()
            .unwrap_or_else(|| format!("{:016x}", id.into_u64()));
        let event_id = attributes
            .get("adk.agent.event_id")
            .cloned()
            .unwrap_or_else(|| format!("{:016x}", id.into_u64()));

        attributes.insert("span_name".to_string(), span_name.clone());
        attributes.insert("trace_id".to_string(), invocation_id); // Group by invocation
        attributes.insert("span_id".to_string(), event_id); // Unique per span
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
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn test_conversation_id_propagates_to_child_spans() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let layer = AdkSpanLayer::new(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let parent = tracing::info_span!(
                "agent.execute",
                "adk.agent.event_id" = "evt-parent",
                "adk.agent.invocation_id" = "inv-1",
                "adk.agent.session_id" = "session-1",
                "gen_ai.conversation.id" = "session-1",
                "agent.name" = "test-agent"
            );

            let _parent_guard = parent.enter();

            let child = tracing::info_span!(
                "call_llm",
                "adk.agent.event_id" = "evt-child",
                "adk.agent.llm_request" = "{}"
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
    fn test_no_duplicate_spans_in_session_trace() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let event_id = "evt-duplicate";
        let session_id = "session-duplicate";

        // First span
        {
            let layer = AdkSpanLayer::new(exporter.clone());
            let subscriber = tracing_subscriber::registry().with(layer);
            tracing::subscriber::with_default(subscriber, || {
                let span1 = tracing::info_span!(
                    "agent.execute",
                    "adk.agent.event_id" = event_id,
                    "adk.agent.session_id" = session_id,
                    "otel.name" = "agent.execute"
                );
                let _guard1 = span1.enter();
            });
        }

        // Second span (duplicate event_id)
        {
            let layer = AdkSpanLayer::new(exporter.clone());
            let subscriber = tracing_subscriber::registry().with(layer);
            tracing::subscriber::with_default(subscriber, || {
                let span2 = tracing::info_span!(
                    "agent.execute",
                    "adk.agent.event_id" = event_id,
                    "adk.agent.session_id" = session_id,
                    "otel.name" = "agent.execute"
                );
                let _guard2 = span2.enter();
            });
        }

        let trace = exporter.get_session_trace(session_id);
        assert_eq!(trace.len(), 1, "Should have only 1 span for the session");
        assert_eq!(trace[0].get("adk.agent.event_id").map(String::as_str), Some(event_id));
    }
}
