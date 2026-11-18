//! The GenAI spans as an OpenTelemetry exporter receives them, checked against the
//! conventions of `semantic-conventions-genai` (`adk_telemetry::semconv::SCHEMA_URL`).

#![cfg(all(feature = "otlp", feature = "genai-semconv"))]

use adk_telemetry::config::{CaptureContent, SemconvConfig};
use adk_telemetry::events::{ContentEventEmitter, InferenceDetails};
use adk_telemetry::{
    LlmUsage, agent_run_span, llm_generate_span, record_llm_usage, record_str_array,
    tool_execute_span,
};
use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry::{Array, StringValue, Value};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_subscriber::layer::SubscriberExt;

/// Runs `body` under an OpenTelemetry layer and returns the spans it exported.
fn exported(body: impl FnOnce()) -> Vec<SpanData> {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder().with_simple_exporter(exporter.clone()).build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("adk-test")));
    tracing::subscriber::with_default(subscriber, body);
    provider.force_flush().unwrap();
    exporter.get_finished_spans().unwrap()
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes.iter().find(|kv| kv.key.as_str() == key).map(|kv| &kv.value)
}

fn keys(span: &SpanData) -> Vec<&str> {
    span.attributes.iter().map(|kv| kv.key.as_str()).collect()
}

fn strings(values: &[&str]) -> Value {
    Value::Array(Array::String(values.iter().map(|v| StringValue::from(v.to_string())).collect()))
}

#[test]
fn an_inference_span_follows_the_client_inference_conventions() {
    let spans = exported(|| {
        let span = llm_generate_span("anthropic", "claude-sonnet-5-5", true);
        let _entered = span.enter();
        record_llm_usage(&LlmUsage {
            input_tokens: 120,
            output_tokens: 40,
            total_tokens: 160,
            cache_read_tokens: Some(100),
            cache_creation_tokens: Some(20),
            thinking_tokens: Some(12),
            audio_input_tokens: None,
            audio_output_tokens: None,
        });
        span.record("gen_ai.response.model", "claude-sonnet-5-5-20261101");
        record_str_array(&span, "gen_ai.response.finish_reasons", &["tool_call"]);
    });
    let [span] = spans.as_slice() else { panic!("one span: {spans:?}") };
    assert_eq!(span.name, "chat claude-sonnet-5-5");
    assert_eq!(span.span_kind, SpanKind::Client);
    for (key, value) in [
        ("gen_ai.operation.name", Value::from("chat")),
        ("gen_ai.provider.name", Value::from("anthropic")),
        ("gen_ai.request.model", Value::from("claude-sonnet-5-5")),
        ("gen_ai.request.stream", Value::from(true)),
        ("gen_ai.response.model", Value::from("claude-sonnet-5-5-20261101")),
        ("gen_ai.usage.input_tokens", Value::from(120_i64)),
        ("gen_ai.usage.output_tokens", Value::from(40_i64)),
        ("gen_ai.usage.cache_read.input_tokens", Value::from(100_i64)),
        ("gen_ai.usage.cache_write.input_tokens", Value::from(20_i64)),
        ("gen_ai.usage.reasoning.output_tokens", Value::from(12_i64)),
        ("gen_ai.response.finish_reasons", strings(&["tool_call"])),
    ] {
        assert_eq!(attr(span, key), Some(&value), "{key}: {:?}", span.attributes);
    }
    let keys = keys(span);
    for retired in [
        "gen_ai.system",
        "gen_ai.usage.total_tokens",
        "gen_ai.usage.cache_read_tokens",
        "gen_ai.usage.cache_creation_tokens",
        "gen_ai.usage.thinking_tokens",
        "gen_ai.input.messages",
    ] {
        assert!(!keys.contains(&retired), "{retired} is not emitted: {keys:?}");
    }
    assert_eq!(
        keys.iter().filter(|key| **key == "gen_ai.response.finish_reasons").count(),
        1,
        "the array is exported once"
    );
}

#[test]
fn gemini_calls_are_generate_content_and_a_unary_call_has_no_stream_attribute() {
    let spans = exported(|| {
        drop(llm_generate_span("gemini", "gemini-3.7-flash", false));
        drop(llm_generate_span("gemini-vertex", "gemini-3.7-flash", false));
        drop(llm_generate_span("mistral", "mistral-large", false));
    });
    let names: Vec<_> = spans.iter().map(|span| span.name.to_string()).collect();
    assert_eq!(
        names,
        [
            "generate_content gemini-3.7-flash",
            "generate_content gemini-3.7-flash",
            "chat mistral-large"
        ]
    );
    let providers: Vec<_> =
        spans.iter().map(|span| attr(span, "gen_ai.provider.name").cloned()).collect();
    assert_eq!(
        providers,
        [
            Some(Value::from("gcp.gemini")),
            Some(Value::from("gcp.vertex_ai")),
            Some(Value::from("mistral"))
        ]
    );
    assert!(spans.iter().all(|span| attr(span, "gen_ai.request.stream").is_none()));
}

#[test]
fn tool_and_agent_spans_follow_the_execute_tool_and_invoke_agent_conventions() {
    let spans = exported(|| {
        let agent = agent_run_span("weather_agent", "inv-1");
        agent.record("gen_ai.conversation.id", "session-1");
        let _agent = agent.enter();
        let tool = adk_telemetry::semconv::tool_call_semconv_span("get_weather", Some("toolu_1"));
        tool.record("gen_ai.tool.description", "Weather for a city");
    });
    let tool = spans.iter().find(|span| span.name == "execute_tool get_weather").expect("tool");
    assert_eq!(tool.span_kind, SpanKind::Internal);
    for (key, value) in [
        ("gen_ai.operation.name", "execute_tool"),
        ("gen_ai.tool.name", "get_weather"),
        ("gen_ai.tool.type", "function"),
        ("gen_ai.tool.call.id", "toolu_1"),
        ("gen_ai.tool.description", "Weather for a city"),
    ] {
        assert_eq!(attr(tool, key), Some(&Value::from(value)), "{key}");
    }
    let agent = spans.iter().find(|span| span.name == "invoke_agent weather_agent").expect("agent");
    assert_eq!(agent.span_kind, SpanKind::Internal);
    assert_eq!(attr(agent, "gen_ai.operation.name"), Some(&Value::from("invoke_agent")));
    assert_eq!(attr(agent, "gen_ai.agent.name"), Some(&Value::from("weather_agent")));
    assert_eq!(attr(agent, "gen_ai.conversation.id"), Some(&Value::from("session-1")));
    assert_eq!(tool.parent_span_id, agent.span_context.span_id());
    let _ = tool_execute_span;
}

#[test]
fn content_goes_on_the_details_event_only_when_capture_says_so() {
    let details = InferenceDetails {
        input_messages: Some(
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#.into(),
        ),
        ..Default::default()
    };
    let spans = exported(|| {
        let span = llm_generate_span("openai", "gpt-6.1-sol", false);
        ContentEventEmitter::emit_inference_details(&SemconvConfig::default(), &span, &details);
        let config = SemconvConfig { capture_content: CaptureContent::EventOnly };
        ContentEventEmitter::emit_inference_details(&config, &span, &details);
    });
    let [span] = spans.as_slice() else { panic!("one span") };
    let events: Vec<_> = span.events.iter().collect();
    assert_eq!(events.len(), 1, "no event without capture: {events:?}");
    assert_eq!(events[0].name, "gen_ai.client.inference.operation.details");
    let input = events[0]
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "gen_ai.input.messages")
        .map(|kv| kv.value.as_str().to_string());
    assert_eq!(input.as_deref(), details.input_messages.as_deref());
    assert!(attr(span, "gen_ai.input.messages").is_none(), "event only, not the span");
}
