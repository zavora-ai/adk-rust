//! Stream wrappers that attribute, price and record token usage.
//!
//! Every provider in this crate passes its response stream through
//! [`with_priced_usage_tracking`] once in `generate_content`, so each response
//! carries [`LlmResponse::provider`] and [`LlmResponse::model`], usage carries
//! a [`UsageMetadata::cost`](adk_core::UsageMetadata::cost) whenever the model is priced, and the active
//! tracing span records standardized `gen_ai.usage.*` fields through
//! [`adk_telemetry::record_llm_usage`]. Each provider-executed built-in tool use
//! on a final response is logged at `info` with its kind and count.

use crate::pricing::PricingCatalog;
use adk_core::{LlmResponse, LlmResponseStream, UsageMetadata};
use futures::StreamExt;
use std::pin::Pin;
use tracing::Span;

/// Keep one verified final response until its usage trailer or protocol end.
/// The absolute grace starts at completion, not at the last heartbeat. Active
/// generation remains governed by the transport's byte-idle timeout.
#[cfg(any(feature = "openai", feature = "deepseek", feature = "gemini"))]
pub(crate) fn with_terminal_usage(mut stream: LlmResponseStream) -> LlmResponseStream {
    Box::pin(async_stream::stream! {
        let mut final_response: Option<LlmResponse> = None;
        let mut deadline = None;
        loop {
            let next = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, stream.next()).await {
                    Ok(next) => next,
                    Err(_) => break,
                },
                None => stream.next().await,
            };
            match next {
                Some(Ok(response)) => {
                    if response.error_code.is_some() || response.error_message.is_some() || response.interrupted {
                        if final_response.is_some() { break; }
                        yield Ok(response);
                        return;
                    }
                    if let Some(pending) = final_response.as_mut() {
                        if response.usage_metadata.is_some() {
                            pending.usage_metadata = response.usage_metadata;
                            yield Ok(final_response.take().expect("pending final response"));
                            return;
                        }
                    } else if !response.partial && response.finish_reason.is_some() {
                        if response.usage_metadata.is_some() {
                            yield Ok(response);
                            return;
                        }
                        final_response = Some(response);
                        deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
                    } else {
                        yield Ok(response);
                    }
                }
                Some(Err(error)) => {
                    // The runner already stops at a completed response. Observing an
                    // optional usage trailer must not turn that answer into a failure.
                    if final_response.is_some() { break; }
                    yield Err(error);
                    return;
                }
                None => break,
            }
        }
        if let Some(response) = final_response {
            yield Ok(response);
        }
    })
}

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
///
/// This wrapper neither attributes nor prices responses; providers use
/// [`with_priced_usage_tracking`].
pub fn with_usage_tracking(stream: LlmResponseStream, span: Span) -> LlmResponseStream {
    let tracked = stream.map(move |result| {
        if let Ok(ref response) = result {
            record_usage_from_response(response, &span);
        }
        result
    });
    Box::pin(tracked) as Pin<Box<_>>
}

/// Attributes, prices and records the usage of every response in `stream`.
///
/// For each response:
///
/// 1. `provider` and `model` are set when the response does not already carry
///    them, so a model version reported by the provider wins over the
///    configured identifier.
/// 2. `usage_metadata.cost` is filled from [`PricingCatalog::standard`] when
///    the provider did not report a cost. It stays `None` for unpriced models.
/// 3. The usage is recorded on `span`, as by [`with_usage_tracking`].
///
/// # Example
///
/// ```
/// use adk_core::{LlmResponse, LlmResponseStream, UsageMetadata};
/// use adk_model::usage_tracking::with_priced_usage_tracking;
/// use futures::StreamExt;
///
/// # futures::executor::block_on(async {
/// let response = LlmResponse {
///     usage_metadata: Some(UsageMetadata {
///         prompt_token_count: 1_000_000,
///         total_token_count: 1_000_000,
///         ..Default::default()
///     }),
///     ..Default::default()
/// };
/// let stream: LlmResponseStream = Box::pin(futures::stream::iter(vec![Ok(response)]));
/// let mut priced =
///     with_priced_usage_tracking(stream, tracing::Span::none(), "openai", "gpt-4.1");
/// let response = priced.next().await.unwrap().unwrap();
/// assert_eq!(response.provider.as_deref(), Some("openai"));
/// assert_eq!(response.model.as_deref(), Some("gpt-4.1"));
/// assert_eq!(response.usage_metadata.unwrap().cost, Some(2.0));
/// # });
/// ```
pub fn with_priced_usage_tracking(
    stream: LlmResponseStream,
    span: Span,
    provider: impl Into<String>,
    model: impl Into<String>,
) -> LlmResponseStream {
    let provider = provider.into();
    let model = model.into();
    let tracked = stream.map(move |result| {
        result.map(|mut response| {
            attribute_and_price(&mut response, &provider, &model);
            record_usage_from_response(&response, &span);
            response
        })
    });
    Box::pin(tracked) as Pin<Box<_>>
}

/// Sets `provider`/`model` when absent and fills a missing cost from the catalog.
pub(crate) fn attribute_and_price(response: &mut LlmResponse, provider: &str, model: &str) {
    if response.provider.is_none() {
        response.provider = Some(provider.to_string());
    }
    if response.model.is_none() {
        response.model = Some(model.to_string());
    }
    let Some(usage) = response.usage_metadata.as_mut() else {
        return;
    };
    if usage.cost.is_some() {
        return;
    }
    let catalog = PricingCatalog::standard();
    let provider = response.provider.as_deref();
    usage.cost = response
        .model
        .as_deref()
        .and_then(|reported| catalog.cost_usd(provider, reported, usage))
        .or_else(|| catalog.cost_usd(provider, model, usage));
}

fn record_usage_from_response(response: &LlmResponse, span: &Span) {
    if let Some(ref usage) = response.usage_metadata {
        let _entered = span.enter();
        record_usage(usage);
        // Partial chunks repeat cumulative usage; the final response reports once.
        if !response.partial {
            for used in &usage.builtin_tool_uses {
                tracing::info!(
                    gen_ai.provider.name = response.provider.as_deref().unwrap_or_default(),
                    gen_ai.response.model = response.model.as_deref().unwrap_or_default(),
                    builtin_tool.kind = %used.kind,
                    builtin_tool.count = used.count,
                    "provider ran built-in tool"
                );
            }
        }
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
