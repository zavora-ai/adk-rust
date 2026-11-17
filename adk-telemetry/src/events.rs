//! The `gen_ai.client.inference.operation.details` event: an inference's content recorded
//! apart from its span attributes, under [`CaptureContent::EventOnly`] or
//! [`CaptureContent::SpanAndEvent`].
//!
//! The event is a `tracing` event at `DEBUG` inside the inference span; the OpenTelemetry
//! layer exports it as a span event of that name.
//!
//! [`CaptureContent::EventOnly`]: crate::config::CaptureContent::EventOnly
//! [`CaptureContent::SpanAndEvent`]: crate::config::CaptureContent::SpanAndEvent

use tracing::Span;

use crate::config::SemconvConfig;

/// The content of one inference, each field JSON in the conventions' message schema.
///
/// # Example
/// ```
/// use adk_telemetry::events::InferenceDetails;
///
/// let details = InferenceDetails {
///     input_messages: Some(r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#.into()),
///     ..Default::default()
/// };
/// assert!(details.output_messages.is_none());
/// ```
#[derive(Debug, Clone, Default)]
pub struct InferenceDetails {
    /// `gen_ai.system_instructions`.
    pub system_instructions: Option<String>,
    /// `gen_ai.input.messages`.
    pub input_messages: Option<String>,
    /// `gen_ai.output.messages`.
    pub output_messages: Option<String>,
    /// `gen_ai.tool.definitions`.
    pub tool_definitions: Option<String>,
}

/// Emits the `gen_ai.client.inference.operation.details` event.
pub struct ContentEventEmitter;

impl ContentEventEmitter {
    /// Emits `details` as a `gen_ai.client.inference.operation.details` event inside `span`,
    /// when `config` captures content on events.
    ///
    /// # Example
    /// ```
    /// use adk_telemetry::config::{CaptureContent, SemconvConfig};
    /// use adk_telemetry::events::{ContentEventEmitter, InferenceDetails};
    ///
    /// let config = SemconvConfig { capture_content: CaptureContent::EventOnly };
    /// let span = adk_telemetry::llm_generate_span("anthropic", "claude-sonnet-5-5", false);
    /// ContentEventEmitter::emit_inference_details(&config, &span, &InferenceDetails::default());
    /// ```
    pub fn emit_inference_details(config: &SemconvConfig, span: &Span, details: &InferenceDetails) {
        if !config.capture_content.on_events() || span.is_none() {
            return;
        }
        let field = |value: &Option<String>| value.clone().unwrap_or_default();
        span.in_scope(|| {
            tracing::debug!(
                gen_ai.system_instructions = field(&details.system_instructions),
                gen_ai.input.messages = field(&details.input_messages),
                gen_ai.output.messages = field(&details.output_messages),
                gen_ai.tool.definitions = field(&details.tool_definitions),
                "gen_ai.client.inference.operation.details"
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitting_without_capture_is_a_noop() {
        let config = SemconvConfig::default();
        ContentEventEmitter::emit_inference_details(
            &config,
            &Span::none(),
            &InferenceDetails::default(),
        );
    }
}
