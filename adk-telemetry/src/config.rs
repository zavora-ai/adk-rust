//! Runtime configuration for semantic convention emission.
//!
//! Controls opt-in behaviors that cannot be gated at compile time, such as capturing the
//! conversation on GenAI spans and events.

use std::sync::atomic::{AtomicU8, Ordering};

/// The environment variable the GenAI conventions name for content capture, read when the
/// process has not set a mode with [`set_capture_content`]. Values, compared without regard to
/// case: `NO_CONTENT` (the default), `SPAN_ONLY`, `EVENT_ONLY`, `SPAN_AND_EVENT`; anything
/// else reads as `NO_CONTENT`.
pub const CAPTURE_CONTENT_ENV: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT";

/// Where GenAI instrumentation records the conversation: the system instructions, input and
/// output messages, tool definitions, and tool call arguments and results.
///
/// Off by default: prompts, answers and tool payloads can hold personal data and secrets.
///
/// # Example
///
/// ```
/// use adk_telemetry::config::CaptureContent;
///
/// assert_eq!(CaptureContent::parse("span_and_event"), CaptureContent::SpanAndEvent);
/// assert_eq!(CaptureContent::parse("yes"), CaptureContent::NoContent);
/// assert!(CaptureContent::SpanOnly.on_spans() && !CaptureContent::SpanOnly.on_events());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureContent {
    /// No content is recorded.
    #[default]
    NoContent,
    /// Content is recorded as attributes of the GenAI spans.
    SpanOnly,
    /// Content is recorded on the `gen_ai.client.inference.operation.details` event.
    EventOnly,
    /// Both.
    SpanAndEvent,
}

impl CaptureContent {
    /// The mode `value` names, as [`CAPTURE_CONTENT_ENV`] spells it.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_uppercase().as_str() {
            "SPAN_ONLY" => Self::SpanOnly,
            "EVENT_ONLY" => Self::EventOnly,
            "SPAN_AND_EVENT" => Self::SpanAndEvent,
            _ => Self::NoContent,
        }
    }

    /// Whether content goes on span attributes.
    pub fn on_spans(self) -> bool {
        matches!(self, Self::SpanOnly | Self::SpanAndEvent)
    }

    /// Whether content goes on the `gen_ai.client.inference.operation.details` event.
    pub fn on_events(self) -> bool {
        matches!(self, Self::EventOnly | Self::SpanAndEvent)
    }

    fn code(self) -> u8 {
        match self {
            Self::NoContent => 1,
            Self::SpanOnly => 2,
            Self::EventOnly => 3,
            Self::SpanAndEvent => 4,
        }
    }
}

/// No mode set in process: read the environment.
const UNSET: u8 = 0;

static CAPTURE: AtomicU8 = AtomicU8::new(UNSET);

/// Sets the content capture mode for this process, overriding [`CAPTURE_CONTENT_ENV`].
///
/// # Example
///
/// ```
/// use adk_telemetry::config::{CaptureContent, capture_content, set_capture_content};
///
/// set_capture_content(CaptureContent::SpanOnly);
/// assert_eq!(capture_content(), CaptureContent::SpanOnly);
/// set_capture_content(CaptureContent::NoContent);
/// assert_eq!(capture_content(), CaptureContent::NoContent);
/// ```
pub fn set_capture_content(mode: CaptureContent) {
    CAPTURE.store(mode.code(), Ordering::Relaxed);
}

/// The content capture mode: the one [`set_capture_content`] set, or else
/// [`CAPTURE_CONTENT_ENV`]'s.
pub fn capture_content() -> CaptureContent {
    match CAPTURE.load(Ordering::Relaxed) {
        1 => CaptureContent::NoContent,
        2 => CaptureContent::SpanOnly,
        3 => CaptureContent::EventOnly,
        4 => CaptureContent::SpanAndEvent,
        _ => std::env::var(CAPTURE_CONTENT_ENV)
            .map(|value| CaptureContent::parse(&value))
            .unwrap_or_default(),
    }
}

/// Runtime configuration for semantic convention emission.
///
/// # Example
/// ```
/// use adk_telemetry::config::{CaptureContent, SemconvConfig};
///
/// // Default: content capture disabled
/// let config = SemconvConfig::default();
/// assert_eq!(config.capture_content, CaptureContent::NoContent);
///
/// // Record the conversation on spans while debugging
/// let debug_config = SemconvConfig { capture_content: CaptureContent::SpanOnly };
/// ```
#[derive(Debug, Clone, Default)]
pub struct SemconvConfig {
    /// Where the conversation is recorded; [`CaptureContent::NoContent`] by default.
    pub capture_content: CaptureContent,
}

impl SemconvConfig {
    /// The process's setting: [`capture_content`].
    pub fn from_process() -> Self {
        Self { capture_content: capture_content() }
    }

    /// Applies this configuration to the process ([`set_capture_content`]).
    pub fn install(&self) {
        set_capture_content(self.capture_content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_disables_content_capture() {
        assert_eq!(SemconvConfig::default().capture_content, CaptureContent::NoContent);
    }

    #[test]
    fn modes_parse_as_the_conventions_spell_them() {
        for (value, mode) in [
            ("NO_CONTENT", CaptureContent::NoContent),
            ("span_only", CaptureContent::SpanOnly),
            ("Event_Only", CaptureContent::EventOnly),
            (" SPAN_AND_EVENT ", CaptureContent::SpanAndEvent),
            ("true", CaptureContent::NoContent),
            ("", CaptureContent::NoContent),
        ] {
            assert_eq!(CaptureContent::parse(value), mode, "{value}");
        }
    }
}
