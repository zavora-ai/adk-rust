//! LangSmith tracing layer implementation
use crate::visitor::StringVisitor;
#[cfg(feature = "langsmith")]
use langsmith_rust::{RunType, Tracer};
use serde_json::json;
use tracing::{Id, Subscriber};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

#[cfg(feature = "langsmith")]
/// Tracing layer that captures spans and exports them to LangSmith
pub struct LangSmithLayer;

#[cfg(feature = "langsmith")]
impl Default for LangSmithLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl LangSmithLayer {
    pub fn new() -> Self {
        Self
    }

    fn map_run_type(span_name: &str) -> RunType {
        match span_name {
            "agent.execute" => RunType::Chain,
            "call_llm" => RunType::Llm,
            "send_data" => RunType::Chain,
            s if s.starts_with("execute_tool") => RunType::Tool,
            _ => RunType::Chain,
        }
    }
}

#[cfg(feature = "langsmith")]
struct LangSmithRun {
    tracer: Tracer,
}

#[cfg(feature = "langsmith")]
impl<S> Layer<S> for LangSmithLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span = ctx.span(id).expect("Span not found");
        let mut extensions = span.extensions_mut();

        // Capture fields
        let mut visitor = StringVisitor::default();
        attrs.record(&mut visitor);
        let fields_map = visitor.0;

        let span_name = span.metadata().name();
        let run_type = Self::map_run_type(span_name);

        let inputs = json!(fields_map);

        let mut tracer = if let Some(parent) = span.parent() {
            if let Some(parent_run) = parent.extensions().get::<LangSmithRun>() {
                parent_run.tracer.create_child(span_name, run_type, inputs)
            } else {
                Tracer::new(span_name, run_type, inputs)
            }
        } else {
            Tracer::new(span_name, run_type, inputs)
        };

        // Add metadata/labels
        if let Some(session_id) = fields_map.get("adk.agent.session_id") {
            tracer = tracer.with_thread_id(session_id.clone());
        }

        let mut tracer_to_post = tracer.clone();
        tokio::spawn(async move {
            let _ = tracer_to_post.post().await;
        });

        extensions.insert(LangSmithRun { tracer });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).expect("Span not found");
        let extensions = span.extensions();

        if let Some(run) = extensions.get::<LangSmithRun>() {
            let mut tracer = run.tracer.clone();

            // For now just setting fixed empty output
            tracer.end(json!({}));

            tokio::spawn(async move {
                let _ = tracer.patch().await;
            });
        }
    }
}
