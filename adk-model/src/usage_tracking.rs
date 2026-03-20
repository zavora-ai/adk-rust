//! Stream wrapper that records token usage on the active tracing span.
//!
//! Wraps any `LlmResponseStream` and intercepts responses carrying `UsageMetadata`,
//! recording standardized `gen_ai.usage.*` fields via [`adk_telemetry::record_llm_usage`].
//!
//! This is applied once per provider in `generate_content`, so every model gets
//! consistent telemetry without duplicating recording logic.

use adk_core::{LlmResponse, LlmResponseStream, UsageMetadata};
use futures::StreamExt;
use std::pin::Pin;
use tracing::Span;

/// Wrap an `LlmResponseStream` so that the last `UsageMetadata` seen is recorded
/// on the provided tracing span when the stream yields it.
///
/// The span is entered briefly for each item that carries usage metadata,
/// ensuring [`adk_telemetry::record_llm_usage`] writes to the correct span
/// regardless of which span is current when the stream is polled.
///
/// For non-streaming (single-response) calls the usage is recorded immediately.
/// For streaming calls the usage typically arrives on the final chunk, so every
/// chunk with `usage_metadata` overwrites the span fields (last write wins).
pub fn with_usage_tracking(stream: LlmResponseStream, span: Span) -> LlmResponseStream {
    let tracked = stream.map(move |result| {
        if let Ok(ref response) = result {
            record_usage_from_response(response, &span);
        }
        result
    });
    Box::pin(tracked) as Pin<Box<_>>
}

fn record_usage_from_response(response: &LlmResponse, span: &Span) {
    if let Some(ref usage) = response.usage_metadata {
        let _entered = span.enter();
        record_usage(usage);
    }
}

fn record_usage(usage: &UsageMetadata) {
    adk_telemetry::record_llm_usage(&adk_telemetry::LlmUsage {
        input_tokens: usage.prompt_token_count,
        output_tokens: usage.candidates_token_count,
        total_tokens: usage.total_token_count,
        cache_read_tokens: usage.cache_read_input_token_count,
        cache_creation_tokens: usage.cache_creation_input_token_count,
        thinking_tokens: usage.thinking_token_count,
        audio_input_tokens: usage.audio_input_token_count,
        audio_output_tokens: usage.audio_output_token_count,
    });
}
