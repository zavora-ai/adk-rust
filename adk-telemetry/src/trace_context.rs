use adk_core::ReadonlyContext;
use tracing::Span;

/// A unified macro to guarantee all ADK spans initialize the correct fields.
/// This eliminates the risk of `span.record` silently failing and enforces DRY.
#[macro_export]
macro_rules! adk_span {
    ($level:expr, $name:expr, $id:expr $(, $($fields:tt)*)?) => {
        tracing::span!(
            $level,
            $name,
            adk.invocation_id = %$id.invocation_id,
            adk.session_id = %$id.session_id,
            adk.user_id = %$id.user_id,
            adk.app_name = %$id.app_name,
            adk.branch = %$id.branch,
            gcp.vertex.invocation_id = %$id.invocation_id,
            gcp.vertex.session_id = %$id.session_id,
            gen_ai.conversation.id = %$id.session_id
            $(, $($fields)*)?
        )
    };
}

/// An extension trait that adds synergistic tracing capabilities to any ADK context.
pub trait TraceContextExt: ReadonlyContext {
    /// Creates a top-level invocation span.
    fn invocation_span(&self) -> Span {
        let span = adk_span!(tracing::Level::INFO, "adk.invocation", self.identity());
        self.record_metadata(&span);
        span
    }

    /// Creates a child span for a specific execution step.
    fn step_span(&self, name: &'static str) -> Span {
        let span = adk_span!(
            tracing::Level::INFO,
            "adk.step",
            self.identity(),
            adk.step.name = name,
            adk.tool.name = tracing::field::Empty
        );
        self.record_metadata(&span);
        span
    }

    /// Creates a specialized span for agent execution.
    fn agent_span(&self) -> Span {
        let id = self.identity();
        let span = adk_span!(
            tracing::Level::INFO,
            "agent.execute",
            id,
            agent.name = %id.agent_name,
            adk.skills.selected_name = tracing::field::Empty,
            adk.skills.selected_id = tracing::field::Empty
        );
        self.record_metadata(&span);
        span
    }

    /// Extends an ALREADY INITIALIZED span with W3C baggage for downstream context propagation.
    fn propagate_adk_identity(&self) {
        let id = self.identity();

        {
            use opentelemetry::{Context, KeyValue, baggage::BaggageExt};

            // ✅ ADK identity attributes injected directly into W3C Baggage
            let cx = Context::current().with_baggage(vec![
                KeyValue::new("adk.invocation_id", id.invocation_id.as_str().to_owned()),
                KeyValue::new("adk.session_id", id.session_id.as_str().to_owned()),
                KeyValue::new("adk.user_id", id.user_id.as_str().to_owned()),
                KeyValue::new("adk.app_name", id.app_name.clone()),
                KeyValue::new("adk.branch", id.branch.clone()),
            ]);

            // The guard implicitly attaches the context to the current thread scope
            let _guard = cx.attach();
        }
    }

    /// Records all key-value pairs from the context's metadata map onto the span.
    fn record_metadata(&self, span: &Span) {
        for (key, value) in self.metadata() {
            span.record(key.as_str(), value.as_str());
        }
    }
}

// Blanket implementation for all types implementing ReadonlyContext.
impl<T: ReadonlyContext> TraceContextExt for T {}
