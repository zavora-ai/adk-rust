//! Tests for standardized token usage tracking across all LLM providers.
//!
//! Verifies that `with_usage_tracking` correctly records `gen_ai.usage.*` fields
//! on the tracing span created by `llm_generate_span`.

use adk_core::{AdkError, Content, LlmResponse, Part, UsageMetadata};
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span;
use tracing_subscriber::layer::SubscriberExt;

#[derive(Debug, Default, Clone)]
struct CapturedFields {
    fields: Arc<Mutex<Vec<(String, i64)>>>,
}

impl Visit for CapturedFields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.lock().unwrap().push((field.name().to_string(), value));
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

struct CapturingLayer {
    captured: CapturedFields,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturingLayer {
    fn on_record(
        &self,
        _id: &span::Id,
        values: &span::Record<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = self.captured.clone();
        values.record(&mut visitor);
    }
}

#[allow(clippy::too_many_arguments)]
fn make_usage(
    prompt: i32,
    completion: i32,
    total: i32,
    cache_read: Option<i32>,
    cache_creation: Option<i32>,
    thinking: Option<i32>,
    audio_in: Option<i32>,
    audio_out: Option<i32>,
) -> UsageMetadata {
    UsageMetadata {
        prompt_token_count: prompt,
        candidates_token_count: completion,
        total_token_count: total,
        cache_read_input_token_count: cache_read,
        cache_creation_input_token_count: cache_creation,
        thinking_token_count: thinking,
        audio_input_token_count: audio_in,
        audio_output_token_count: audio_out,
        ..Default::default()
    }
}

fn response_with_usage(usage: UsageMetadata) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "hello".to_string() }],
        }),
        usage_metadata: Some(usage),
        finish_reason: Some(adk_core::FinishReason::Stop),
        partial: false,
        turn_complete: true,
        ..Default::default()
    }
}

fn partial_response() -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "hel".to_string() }],
        }),
        partial: true,
        ..Default::default()
    }
}

/// Set up a capturing subscriber that persists across async awaits via `set_default`.
fn setup_capture() -> (tracing::dispatcher::DefaultGuard, CapturedFields) {
    let captured = CapturedFields::default();
    let layer = CapturingLayer { captured: captured.clone() };
    let subscriber = tracing_subscriber::registry().with(layer);
    let dispatch = tracing::dispatcher::Dispatch::new(subscriber);
    let guard = tracing::dispatcher::set_default(&dispatch);
    (guard, captured)
}

fn field_map(fields: &[(String, i64)]) -> std::collections::HashMap<&str, i64> {
    fields.iter().map(|(k, v)| (k.as_str(), *v)).collect()
}

/// Verify that with_usage_tracking records all token fields on the span.
#[tokio::test]
async fn test_records_all_fields() {
    let (_guard, captured) = setup_capture();

    let usage = make_usage(100, 50, 150, Some(80), Some(20), Some(10), Some(30), Some(15));
    let stream: adk_core::LlmResponseStream =
        Box::pin(futures::stream::once(async { Ok(response_with_usage(usage)) }));

    let span = adk_telemetry::llm_generate_span("test", "test-model", false);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let results: Vec<_> = tracked.collect().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_ok());

    let fields = captured.fields.lock().unwrap();
    let m = field_map(&fields);
    assert_eq!(m.get("gen_ai.usage.input_tokens"), Some(&100));
    assert_eq!(m.get("gen_ai.usage.output_tokens"), Some(&50));
    assert_eq!(m.get("gen_ai.usage.total_tokens"), Some(&150));
    assert_eq!(m.get("gen_ai.usage.cache_read_tokens"), Some(&80));
    assert_eq!(m.get("gen_ai.usage.cache_creation_tokens"), Some(&20));
    assert_eq!(m.get("gen_ai.usage.thinking_tokens"), Some(&10));
    assert_eq!(m.get("gen_ai.usage.audio_input_tokens"), Some(&30));
    assert_eq!(m.get("gen_ai.usage.audio_output_tokens"), Some(&15));
}

/// Verify that partial responses without usage metadata don't record anything.
#[tokio::test]
async fn test_skips_partial_without_usage() {
    let (_guard, captured) = setup_capture();

    let stream: adk_core::LlmResponseStream =
        Box::pin(futures::stream::once(async { Ok(partial_response()) }));

    let span = adk_telemetry::llm_generate_span("test", "test-model", true);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let results: Vec<_> = tracked.collect().await;
    assert_eq!(results.len(), 1);

    let fields = captured.fields.lock().unwrap();
    let usage_fields: Vec<_> =
        fields.iter().filter(|(k, _)| k.starts_with("gen_ai.usage")).collect();
    assert!(usage_fields.is_empty(), "expected no usage fields, got: {usage_fields:?}");
}

/// Verify streaming: last chunk with usage wins (overwrites earlier values).
#[tokio::test]
async fn test_last_write_wins_for_streaming() {
    let (_guard, captured) = setup_capture();

    let final_usage = make_usage(200, 100, 300, None, None, None, None, None);
    let stream: adk_core::LlmResponseStream = Box::pin(futures::stream::iter(vec![
        Ok(partial_response()),
        Ok(response_with_usage(final_usage)),
    ]));

    let span = adk_telemetry::llm_generate_span("test", "test-model", true);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let results: Vec<_> = tracked.collect().await;
    assert_eq!(results.len(), 2);

    let fields = captured.fields.lock().unwrap();
    let m = field_map(&fields);
    assert_eq!(m.get("gen_ai.usage.input_tokens"), Some(&200));
    assert_eq!(m.get("gen_ai.usage.output_tokens"), Some(&100));
    assert_eq!(m.get("gen_ai.usage.total_tokens"), Some(&300));
}

/// Verify that optional fields (cache, thinking, audio) are omitted when None.
#[tokio::test]
async fn test_omits_none_optional_fields() {
    let (_guard, captured) = setup_capture();

    let usage = make_usage(50, 25, 75, None, None, None, None, None);
    let stream: adk_core::LlmResponseStream =
        Box::pin(futures::stream::once(async { Ok(response_with_usage(usage)) }));

    let span = adk_telemetry::llm_generate_span("test", "test-model", false);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let _: Vec<_> = tracked.collect().await;

    let fields = captured.fields.lock().unwrap();
    let names: Vec<&str> = fields.iter().map(|(k, _)| k.as_str()).collect();
    assert!(names.contains(&"gen_ai.usage.input_tokens"));
    assert!(names.contains(&"gen_ai.usage.output_tokens"));
    assert!(names.contains(&"gen_ai.usage.total_tokens"));
    assert!(!names.contains(&"gen_ai.usage.cache_read_tokens"));
    assert!(!names.contains(&"gen_ai.usage.cache_creation_tokens"));
    assert!(!names.contains(&"gen_ai.usage.thinking_tokens"));
    assert!(!names.contains(&"gen_ai.usage.audio_input_tokens"));
    assert!(!names.contains(&"gen_ai.usage.audio_output_tokens"));
}

/// Verify that error responses in the stream don't cause panics.
#[tokio::test]
async fn test_handles_error_responses() {
    let (_guard, captured) = setup_capture();

    let stream: adk_core::LlmResponseStream =
        Box::pin(futures::stream::once(async { Err(AdkError::model("test error")) }));

    let span = adk_telemetry::llm_generate_span("test", "test-model", false);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let results: Vec<_> = tracked.collect().await;
    assert_eq!(results.len(), 1);
    assert!(results[0].is_err());

    let fields = captured.fields.lock().unwrap();
    let usage_fields: Vec<_> =
        fields.iter().filter(|(k, _)| k.starts_with("gen_ai.usage")).collect();
    assert!(usage_fields.is_empty(), "no usage fields should be recorded on error");
}

/// Verify that llm_generate_span creates a span with the correct static fields.
#[tokio::test]
async fn test_span_has_correct_static_fields() {
    let captured_new = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let captured_clone = Arc::clone(&captured_new);

    struct StaticFieldLayer {
        captured: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct StaticVisitor {
        fields: Vec<(String, String)>,
    }

    impl Visit for StaticVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields.push((field.name().to_string(), value.to_string()));
        }
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields.push((field.name().to_string(), value.to_string()));
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields.push((field.name().to_string(), format!("{value:?}")));
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for StaticFieldLayer {
        fn on_new_span(
            &self,
            attrs: &span::Attributes<'_>,
            _id: &span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = StaticVisitor { fields: Vec::new() };
            attrs.record(&mut visitor);
            self.captured.lock().unwrap().extend(visitor.fields);
        }
    }

    let layer = StaticFieldLayer { captured: captured_clone };
    let subscriber = tracing_subscriber::registry().with(layer);
    let dispatch = tracing::dispatcher::Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);

    let _span = adk_telemetry::llm_generate_span("openai", "gpt-5-mini", true);

    let fields = captured_new.lock().unwrap();
    let m: std::collections::HashMap<&str, &str> =
        fields.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    assert_eq!(m.get("gen_ai.system"), Some(&"openai"));
    assert_eq!(m.get("gen_ai.request.model"), Some(&"gpt-5-mini"));
    assert_eq!(m.get("gen_ai.request.stream"), Some(&"true"));
    assert_eq!(m.get("otel.kind"), Some(&"client"));
}

/// Verify audio-only usage (no cache/thinking) records correctly.
#[tokio::test]
async fn test_audio_tokens_only() {
    let (_guard, captured) = setup_capture();

    let usage = make_usage(10, 20, 30, None, None, None, Some(500), Some(400));
    let stream: adk_core::LlmResponseStream =
        Box::pin(futures::stream::once(async { Ok(response_with_usage(usage)) }));

    let span = adk_telemetry::llm_generate_span("test", "test-model", false);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let _: Vec<_> = tracked.collect().await;

    let fields = captured.fields.lock().unwrap();
    let m = field_map(&fields);
    assert_eq!(m.get("gen_ai.usage.input_tokens"), Some(&10));
    assert_eq!(m.get("gen_ai.usage.output_tokens"), Some(&20));
    assert_eq!(m.get("gen_ai.usage.audio_input_tokens"), Some(&500));
    assert_eq!(m.get("gen_ai.usage.audio_output_tokens"), Some(&400));
    assert!(!m.contains_key("gen_ai.usage.cache_read_tokens"));
    assert!(!m.contains_key("gen_ai.usage.thinking_tokens"));
}

/// Verify multiple usage updates in a stream accumulate correctly (last wins).
#[tokio::test]
async fn test_multiple_usage_updates_last_wins() {
    let (_guard, captured) = setup_capture();

    let early_usage = make_usage(10, 5, 15, None, None, None, None, None);
    let final_usage = make_usage(100, 50, 150, Some(30), None, Some(5), Some(10), Some(8));
    let stream: adk_core::LlmResponseStream = Box::pin(futures::stream::iter(vec![
        Ok(response_with_usage(early_usage)),
        Ok(partial_response()),
        Ok(response_with_usage(final_usage)),
    ]));

    let span = adk_telemetry::llm_generate_span("test", "test-model", true);
    let tracked = adk_model::usage_tracking::with_usage_tracking(stream, span);
    let results: Vec<_> = tracked.collect().await;
    assert_eq!(results.len(), 3);

    let fields = captured.fields.lock().unwrap();
    let m = field_map(&fields);
    // Last write wins — final_usage values
    assert_eq!(m.get("gen_ai.usage.input_tokens"), Some(&100));
    assert_eq!(m.get("gen_ai.usage.output_tokens"), Some(&50));
    assert_eq!(m.get("gen_ai.usage.total_tokens"), Some(&150));
    assert_eq!(m.get("gen_ai.usage.cache_read_tokens"), Some(&30));
    assert_eq!(m.get("gen_ai.usage.thinking_tokens"), Some(&5));
    assert_eq!(m.get("gen_ai.usage.audio_input_tokens"), Some(&10));
    assert_eq!(m.get("gen_ai.usage.audio_output_tokens"), Some(&8));
}
