use std::collections::{BTreeMap, HashMap};
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
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

/// Spans an [`AdkSpanExporter`] retains by default before evicting the oldest.
pub const DEFAULT_MAX_SPANS: usize = 10_000;

/// ADK-Go style span exporter that retains runtime spans in memory.
///
/// Spans are keyed by a stable span ID while preserving the originating ADK
/// event ID as an attribute. This lets multiple runtime operations describe the
/// same event without overwriting one another.
///
/// Retention is bounded: once [`max_spans`](Self::with_max_spans) spans are held,
/// storing another evicts the least recently stored one, and with a
/// [`ttl`](Self::with_ttl) set, spans older than it are dropped. The defaults are
/// [`DEFAULT_MAX_SPANS`] spans and no TTL.
///
/// # Example
///
/// ```
/// use std::collections::HashMap;
/// use std::time::Duration;
///
/// use adk_telemetry::{AdkSpanExporter, SpanSink};
///
/// let exporter = AdkSpanExporter::new().with_max_spans(2).with_ttl(Duration::from_secs(600));
/// for id in ["a", "b", "c"] {
///     let attributes = HashMap::from([
///         ("span_id".to_string(), id.to_string()),
///         ("gcp.vertex.agent.event_id".to_string(), id.to_string()),
///     ]);
///     exporter.export_span("call_llm", attributes);
/// }
///
/// // The oldest span was evicted to stay within two.
/// assert!(exporter.get_trace_by_event_id("a").is_none());
/// assert!(exporter.get_trace_by_event_id("c").is_some());
/// ```
#[derive(Debug, Clone)]
pub struct AdkSpanExporter {
    /// Retained spans with their eviction order.
    store: Arc<RwLock<SpanStore>>,
    /// Whether this exporter has observed at least one retained runtime span.
    collecting: Arc<AtomicBool>,
    max_spans: usize,
    ttl: Option<Duration>,
}

/// Spans keyed by span ID, plus the order they were stored in.
#[derive(Debug, Default)]
struct SpanStore {
    spans: HashMap<String, StoredSpan>,
    /// Insertion sequence → span ID; the first entry is the next to evict.
    order: BTreeMap<u64, String>,
    next_seq: u64,
}

#[derive(Debug)]
struct StoredSpan {
    seq: u64,
    stored_at: Instant,
    attributes: HashMap<String, String>,
}

impl SpanStore {
    fn insert(&mut self, key: String, attributes: HashMap<String, String>, max_spans: usize) {
        let seq = self.next_seq;
        self.next_seq += 1;
        if let Some(previous) = self
            .spans
            .insert(key.clone(), StoredSpan { seq, stored_at: Instant::now(), attributes })
        {
            self.order.remove(&previous.seq);
        }
        self.order.insert(seq, key);
        while self.spans.len() > max_spans {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            self.spans.remove(&oldest);
        }
    }

    /// Drops spans stored longer than `ttl` ago.
    fn expire(&mut self, ttl: Option<Duration>) {
        let Some(ttl) = ttl else { return };
        while let Some((_, oldest)) = self.order.first_key_value() {
            let expired = self.spans.get(oldest).is_none_or(|span| span.stored_at.elapsed() > ttl);
            if !expired {
                break;
            }
            if let Some((_, key)) = self.order.pop_first() {
                self.spans.remove(&key);
            }
        }
    }

    fn live<'a>(
        &'a self,
        ttl: Option<Duration>,
    ) -> impl Iterator<Item = (&'a String, &'a HashMap<String, String>)> + 'a {
        self.spans
            .iter()
            .filter(move |(_, span)| ttl.is_none_or(|ttl| span.stored_at.elapsed() <= ttl))
            .map(|(key, span)| (key, &span.attributes))
    }
}

impl Default for AdkSpanExporter {
    fn default() -> Self {
        Self::new()
    }
}

impl AdkSpanExporter {
    /// Creates an empty in-process span exporter that retains up to
    /// [`DEFAULT_MAX_SPANS`] spans, with no TTL.
    pub fn new() -> Self {
        Self {
            store: Arc::new(RwLock::new(SpanStore::default())),
            collecting: Arc::new(AtomicBool::new(false)),
            max_spans: DEFAULT_MAX_SPANS,
            ttl: None,
        }
    }

    /// Sets how many spans are retained before the least recently stored is evicted.
    ///
    /// A value of 0 is treated as 1.
    #[must_use]
    pub fn with_max_spans(mut self, max_spans: usize) -> Self {
        self.max_spans = max_spans.max(1);
        self
    }

    /// Sets how long a span is retained after it is stored.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Returns a snapshot of retained spans keyed by span ID.
    pub fn get_trace_dict(&self) -> HashMap<String, HashMap<String, String>> {
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
        store.live(self.ttl).map(|(key, attributes)| (key.clone(), attributes.clone())).collect()
    }

    /// Returns the first span associated with an ADK event ID.
    pub fn get_trace_by_event_id(&self, event_id: &str) -> Option<HashMap<String, String>> {
        debug!("AdkSpanExporter::get_trace_by_event_id called with event_id: {}", event_id);
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
        let result = store
            .spans
            .get(event_id)
            .filter(|span| self.ttl.is_none_or(|ttl| span.stored_at.elapsed() <= ttl))
            .map(|span| span.attributes.clone())
            .or_else(|| {
                store
                    .live(self.ttl)
                    .find(|(_, attributes)| {
                        attributes.get("gcp.vertex.agent.event_id").is_some_and(|id| id == event_id)
                    })
                    .map(|(_, attributes)| attributes.clone())
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
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());

        let spans: Vec<HashMap<String, String>> = store
            .live(self.ttl)
            .filter(|(_, attributes)| {
                attributes.get("gcp.vertex.agent.session_id").is_some_and(|id| id == session_id)
            })
            .map(|(_, attributes)| attributes.clone())
            .collect();

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
                let mut store = self.store.write().unwrap_or_else(|e| e.into_inner());
                store.expire(self.ttl);
                store.insert(storage_key, attributes, self.max_spans);
                self.collecting.store(true, Ordering::Release);
                debug!("AdkSpanExporter: Span stored, total spans: {}", store.spans.len());
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
            // `adk.app_name` and `adk.user_id` are inherited so every span of a run
            // carries the owner the debug routes check before returning it.
            let context_keys = [
                "gcp.vertex.agent.session_id",
                "gcp.vertex.agent.invocation_id",
                "gcp.vertex.agent.event_id",
                "gen_ai.conversation.id",
                "adk.app_name",
                "adk.user_id",
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
    fn child_spans_inherit_the_run_owner() {
        let exporter = Arc::new(AdkSpanExporter::new());
        let layer = AdkSpanLayer::new(exporter.clone()).with_filter(filter_fn(|metadata| {
            metadata.is_span() && is_runtime_span(metadata.name())
        }));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let run = tracing::info_span!(
                "agent.execute",
                "gcp.vertex.agent.event_id" = "evt-run",
                "gcp.vertex.agent.invocation_id" = "inv-owner",
                "gcp.vertex.agent.session_id" = "session-owner",
                "adk.app_name" = "app",
                "adk.user_id" = "alice"
            );
            let _run = run.enter();
            let call_llm = tracing::info_span!("call_llm", "gcp.vertex.agent.event_id" = "evt-llm");
            let _call_llm = call_llm.enter();
            // A span the layer filters out must not break the chain.
            let inner = tracing::info_span!("provider.request");
            let _inner = inner.enter();
            let _tool = tracing::info_span!("execute_tool lookup").entered();
        });

        let spans = exporter.get_session_trace("session-owner");
        assert_eq!(spans.len(), 3);
        for span in &spans {
            assert_eq!(
                (
                    span.get("adk.user_id").map(String::as_str),
                    span.get("adk.app_name").map(String::as_str)
                ),
                (Some("alice"), Some("app")),
                "{span:?}"
            );
        }
    }

    fn span(id: &str) -> HashMap<String, String> {
        HashMap::from([
            ("span_id".to_string(), id.to_string()),
            ("gcp.vertex.agent.event_id".to_string(), id.to_string()),
            ("gcp.vertex.agent.session_id".to_string(), "session".to_string()),
        ])
    }

    #[test]
    fn retention_is_bounded_by_span_count() {
        let exporter = AdkSpanExporter::new().with_max_spans(2);
        for id in ["a", "b"] {
            exporter.export_span("call_llm", span(id));
        }
        // Storing "a" again makes "b" the least recently stored.
        exporter.export_span("call_llm", span("a"));
        exporter.export_span("call_llm", span("c"));

        let mut retained: Vec<String> = exporter.get_trace_dict().into_keys().collect();
        retained.sort();
        assert_eq!(retained, vec!["a".to_string(), "c".to_string()]);
        assert!(exporter.get_trace_by_event_id("b").is_none());
        assert_eq!(exporter.get_session_trace("session").len(), 2);
    }

    #[test]
    fn spans_expire_after_the_ttl() {
        let exporter = AdkSpanExporter::new().with_ttl(Duration::from_millis(1));
        exporter.export_span("call_llm", span("old"));
        std::thread::sleep(Duration::from_millis(20));

        assert!(exporter.get_trace_by_event_id("old").is_none());
        assert!(exporter.get_session_trace("session").is_empty());

        exporter.export_span("call_llm", span("new"));
        assert_eq!(exporter.store.read().unwrap().spans.len(), 1, "storing expires old spans");
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
