//! # ADK Telemetry Demo
//!
//! Demonstrates ADK telemetry and tracing capabilities using real LLM calls.
//!
//! ## Requirements
//!
//! Set `GOOGLE_API_KEY` or `GEMINI_API_KEY` in your environment (or `.env` file).
//!
//! ## Features demonstrated
//!
//! 1. Console logging initialization with ADK span exporter
//! 2. Structured logging at all levels
//! 3. Pre-configured spans: agent, model, tool, callback, LLM generation
//! 4. Real token usage recording via `with_usage_tracking` (non-streaming + streaming)
//! 5. Context attribute propagation
//! 6. Custom spans with `#[instrument]`
//! 7. Nested span hierarchy (agent → model → tool)
//! 8. ADK span exporter for programmatic span access
//! 9. OpenTelemetry metrics (counters, histograms)

use adk_core::{Content, Llm, LlmRequest};
use adk_model::gemini::GeminiModel;
use adk_telemetry::{
    LlmUsage, add_context_attributes, agent_run_span, callback_span, debug, error, info,
    llm_generate_span, record_llm_usage, tool_execute_span, warn,
};
use futures::StreamExt;
use std::sync::Arc;
use tracing::Instrument;

// ---------------------------------------------------------------------------
// Demo sections
// ---------------------------------------------------------------------------

/// Demo 1: Basic structured logging at all levels.
fn demo_structured_logging() {
    info!("--- demo 1: structured logging ---");
    tracing::trace!(detail = "finest granularity", "trace-level message");
    debug!(component = "demo", "debug-level message");
    info!(status = "running", version = "0.4.1", "info-level message");
    warn!(latency_ms = 250, "warn-level: high latency detected");
    error!(error.code = "TIMEOUT", "error-level: operation timed out");
}

/// Demo 2: Pre-configured agent span with context attributes.
async fn demo_agent_span() {
    info!("--- demo 2: agent span with context ---");

    let span = agent_run_span("weather-agent", "inv-001");
    async {
        add_context_attributes("user-42", "session-abc");
        info!("agent execution started");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        info!("agent execution completed");
    }
    .instrument(span)
    .await;
}

/// Demo 3: Real LLM call (non-streaming) with automatic token usage recording.
///
/// Uses `GeminiModel` which internally creates an `llm_generate_span` and wraps
/// the response stream with `with_usage_tracking`. Token counts appear on the
/// `gen_ai.usage.*` span attributes automatically.
async fn demo_real_llm_non_streaming(model: &Arc<GeminiModel>) {
    info!("--- demo 3: real LLM call (non-streaming) ---");

    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text("What is 2 + 2? Reply in one sentence.")],
        config: None,
        tools: Default::default(),
    };

    match model.generate_content(request, false).await {
        Ok(mut stream) => {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(response) => {
                        let text = response
                            .content
                            .as_ref()
                            .and_then(|c| c.parts.first())
                            .and_then(|p| p.text())
                            .unwrap_or("(no text)");
                        info!(response.text = text, "LLM response received");

                        if let Some(ref usage) = response.usage_metadata {
                            info!(
                                input_tokens = usage.prompt_token_count,
                                output_tokens = usage.candidates_token_count,
                                total_tokens = usage.total_token_count,
                                "token usage (recorded automatically on span)"
                            );
                        }
                    }
                    Err(e) => error!(error = %e, "LLM response error"),
                }
            }
        }
        Err(e) => error!(error = %e, "LLM call failed"),
    }
}

/// Demo 4: Real LLM call (streaming) with incremental token usage recording.
///
/// Streaming calls yield partial chunks. The final chunk carries `UsageMetadata`,
/// which `with_usage_tracking` records on the span automatically.
async fn demo_real_llm_streaming(model: &Arc<GeminiModel>) {
    info!("--- demo 4: real LLM call (streaming) ---");

    let request = LlmRequest {
        model: String::new(),
        contents: vec![
            Content::new("user").with_text("List three benefits of Rust in one sentence each."),
        ],
        config: None,
        tools: Default::default(),
    };

    match model.generate_content(request, true).await {
        Ok(mut stream) => {
            let mut chunk_count = 0u32;
            while let Some(result) = stream.next().await {
                chunk_count += 1;
                match result {
                    Ok(response) => {
                        let text = response
                            .content
                            .as_ref()
                            .and_then(|c| c.parts.first())
                            .and_then(|p| p.text())
                            .unwrap_or("");
                        debug!(
                            chunk = chunk_count,
                            partial = response.partial,
                            text_len = text.len(),
                            "stream chunk"
                        );

                        if let Some(ref usage) = response.usage_metadata {
                            info!(
                                input_tokens = usage.prompt_token_count,
                                output_tokens = usage.candidates_token_count,
                                total_tokens = usage.total_token_count,
                                "final chunk usage (recorded automatically on span)"
                            );
                        }
                    }
                    Err(e) => error!(error = %e, "stream chunk error"),
                }
            }
            info!(total_chunks = chunk_count, "stream completed");
        }
        Err(e) => error!(error = %e, "streaming LLM call failed"),
    }
}

/// Demo 5: Manual LLM usage recording on a custom span.
///
/// Shows how to use `llm_generate_span` + `record_llm_usage` directly
/// if you're building a custom provider or need manual control.
async fn demo_manual_usage_recording() {
    info!("--- demo 5: manual usage recording ---");

    let span = llm_generate_span("custom-provider", "custom-model-v1", false);
    async {
        // Simulate a provider that returns usage data you record yourself
        record_llm_usage(&LlmUsage {
            input_tokens: 150,
            output_tokens: 42,
            total_tokens: 192,
            cache_read_tokens: Some(10),
            thinking_tokens: Some(5),
            ..Default::default()
        });
        info!("manually recorded usage: 150 input, 42 output, 192 total");
    }
    .instrument(span)
    .await;
}

/// Demo 6: Tool execution span.
async fn demo_tool_span() {
    info!("--- demo 6: tool execution span ---");

    let span = tool_execute_span("weather_lookup");
    async {
        let input = serde_json::json!({"city": "Seattle", "units": "metric"});
        // Simulate tool work
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        info!(tool.name = "weather_lookup", input = %input, "tool executed");
    }
    .instrument(span)
    .await;
}

/// Demo 7: Callback span.
fn demo_callback_span() {
    info!("--- demo 7: callback span ---");

    let span = callback_span("before_model");
    let _guard = span.enter();
    info!("callback: validating request before model call");

    let span2 = callback_span("after_agent");
    let _guard2 = span2.enter();
    info!("callback: post-processing agent output");
}

/// Demo 8: Nested span hierarchy (agent → model → tool) with a real LLM call.
async fn demo_nested_hierarchy(model: &Arc<GeminiModel>) {
    info!("--- demo 8: nested span hierarchy ---");

    let agent_span = agent_run_span("orchestrator-agent", "inv-002");
    async {
        info!("orchestrator started");

        // Real model call inside agent span
        let request = LlmRequest {
            model: String::new(),
            contents: vec![Content::new("user").with_text("Say hello in exactly three words.")],
            config: None,
            tools: Default::default(),
        };

        match model.generate_content(request, false).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    if let Ok(response) = result {
                        let text = response
                            .content
                            .as_ref()
                            .and_then(|c| c.parts.first())
                            .and_then(|p| p.text())
                            .unwrap_or("(no text)");
                        info!(response.text = text, "model returned inside agent span");
                    }
                }
            }
            Err(e) => error!(error = %e, "nested model call failed"),
        }

        // Tool call inside agent span
        let tool_span = tool_execute_span("code_executor");
        async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            info!("tool executed inside agent span");
        }
        .instrument(tool_span)
        .await;

        info!("orchestrator completed");
    }
    .instrument(agent_span)
    .await;
}

/// Demo 9: ADK span exporter — programmatic access to captured spans.
async fn demo_adk_exporter(exporter: &Arc<adk_telemetry::AdkSpanExporter>) {
    info!("--- demo 9: ADK span exporter ---");

    let trace_dict = exporter.get_trace_dict();
    info!(captured_spans = trace_dict.len(), "spans in exporter trace dict");

    for (event_id, attrs) in &trace_dict {
        info!(
            event_id = event_id.as_str(),
            span_name = attrs.get("span_name").map(String::as_str).unwrap_or("unknown"),
            "captured span"
        );
    }
}

/// Demo 10: OpenTelemetry metrics.
fn demo_metrics() {
    info!("--- demo 10: OpenTelemetry metrics ---");

    let meter = adk_telemetry::global::meter("telemetry-demo");

    let request_counter =
        meter.u64_counter("adk.llm.requests").with_description("Total LLM requests").build();

    let latency_histogram = meter
        .f64_histogram("adk.llm.latency_ms")
        .with_description("LLM request latency in milliseconds")
        .build();

    let token_counter =
        meter.u64_counter("adk.llm.tokens").with_description("Total tokens consumed").build();

    request_counter.add(1, &[opentelemetry::KeyValue::new("provider", "gemini")]);
    latency_histogram.record(52.3, &[opentelemetry::KeyValue::new("provider", "gemini")]);
    token_counter.add(150, &[opentelemetry::KeyValue::new("type", "input")]);
    token_counter.add(42, &[opentelemetry::KeyValue::new("type", "output")]);

    info!("metrics recorded: 1 request, 52.3ms latency, 192 tokens");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // Initialize with ADK span exporter (captures spans + console output)
    let exporter = adk_telemetry::init_with_adk_exporter("telemetry-demo")
        .expect("failed to initialize telemetry");

    info!("=== ADK Telemetry Demo ===");

    // Set up real Gemini model
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("GOOGLE_API_KEY or GEMINI_API_KEY must be set");

    let model = Arc::new(
        GeminiModel::new(&api_key, "gemini-2.5-flash").expect("failed to create GeminiModel"),
    );

    info!("using model: gemini-2.5-flash\n");

    // Run all demos
    demo_structured_logging();
    demo_agent_span().await;
    demo_real_llm_non_streaming(&model).await;
    demo_real_llm_streaming(&model).await;
    demo_manual_usage_recording().await;
    demo_tool_span().await;
    demo_callback_span();
    demo_nested_hierarchy(&model).await;
    demo_adk_exporter(&exporter).await;
    demo_metrics();

    info!("\n=== all demos completed ===");

    adk_telemetry::shutdown_telemetry();
}
