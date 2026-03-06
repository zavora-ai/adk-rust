//! Tracing utilities for diagnostic logging.
//!
//! This module provides utilities for consistent diagnostic logging across
//! the adk-mistralrs crate, including timing spans for model loading and inference.
//!
//! ## Usage
//!
//! The tracing utilities are automatically used by model implementations.
//! To enable logging output, configure a tracing subscriber in your application:
//!
//! ```rust,ignore
//! use tracing_subscriber::{fmt, prelude::*, EnvFilter};
//!
//! tracing_subscriber::registry()
//!     .with(fmt::layer())
//!     .with(EnvFilter::from_default_env())
//!     .init();
//!
//! // Now model operations will emit structured logs
//! let model = MistralRsModel::from_hf("microsoft/Phi-3.5-mini-instruct").await?;
//! ```
//!
//! ## Log Levels
//!
//! - `INFO`: Model loading start/completion, major operations
//! - `DEBUG`: Configuration details, intermediate steps
//! - `WARN`: Non-fatal issues, fallback behaviors
//! - `ERROR`: Operation failures (also returned as errors)
//!
//! ## Span Fields
//!
//! Common span fields include:
//! - `model`: Model name/ID
//! - `model_source`: Source type (HuggingFace, Local, GGUF, UQFF)
//! - `duration_ms`: Operation duration in milliseconds
//! - `tokens`: Token counts for inference operations

use std::time::Instant;
use tracing::{Level, Span, debug, info, span};

/// A guard that logs the duration of an operation when dropped.
///
/// This is useful for timing operations without manual start/end logging.
///
/// # Example
///
/// ```rust,ignore
/// let _timer = TimingGuard::new("model_loading", "microsoft/Phi-3.5-mini-instruct");
/// // ... loading operation ...
/// // Duration is logged when _timer goes out of scope
/// ```
pub struct TimingGuard {
    operation: &'static str,
    context: String,
    start: Instant,
    span: Span,
}

impl TimingGuard {
    /// Create a new timing guard for an operation.
    ///
    /// # Arguments
    ///
    /// * `operation` - Name of the operation being timed
    /// * `context` - Additional context (e.g., model ID)
    pub fn new(operation: &'static str, context: impl Into<String>) -> Self {
        let context = context.into();        let span = span!(Level::INFO, "timing", operation = operation, context = %context);

        // Enter the span briefly to log the start
        {
            let _enter = span.enter();
            debug!(operation = operation, context = %context, "Starting operation");
        }

        Self { operation, context, start: Instant::now(), span }
    }

    /// Get the elapsed time since the guard was created.
    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        let duration_ms = self.elapsed_ms();
        let _enter = self.span.enter();

        info!(
            operation = self.operation,
            context = %self.context,
            duration_ms = duration_ms,
            "Operation completed"
        );
    }
}

/// Log model loading start with configuration details.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `source_type` - Type of model source (HuggingFace, Local, etc.)
/// * `has_isq` - Whether ISQ quantization is enabled
/// * `has_paged_attn` - Whether PagedAttention is enabled
pub fn log_model_loading_start(
    model_id: &str,
    source_type: &str,
    has_isq: bool,
    has_paged_attn: bool,
) {
    info!(
        model_id = model_id,
        source_type = source_type,
        isq_enabled = has_isq,
        paged_attention = has_paged_attn,
        "Starting model load"
    );
}

/// Log model loading completion with timing.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `duration_ms` - Time taken to load in milliseconds
pub fn log_model_loading_complete(model_id: &str, duration_ms: u64) {
    info!(model_id = model_id, duration_ms = duration_ms, "Model loaded successfully");
}

/// Log inference start with request details.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `message_count` - Number of messages in the request
/// * `streaming` - Whether streaming is enabled
pub fn log_inference_start(model_id: &str, message_count: usize, streaming: bool) {
    debug!(
        model_id = model_id,
        message_count = message_count,
        streaming = streaming,
        "Starting inference"
    );
}

/// Log inference completion with token usage.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `duration_ms` - Time taken for inference in milliseconds
/// * `prompt_tokens` - Number of prompt tokens
/// * `completion_tokens` - Number of completion tokens
pub fn log_inference_complete(
    model_id: &str,
    duration_ms: u64,
    prompt_tokens: i32,
    completion_tokens: i32,
) {
    let total_tokens = prompt_tokens + completion_tokens;
    let tokens_per_sec = if duration_ms > 0 {
        (completion_tokens as f64 / duration_ms as f64) * 1000.0
    } else {
        0.0
    };

    info!(
        model_id = model_id,
        duration_ms = duration_ms,
        prompt_tokens = prompt_tokens,
        completion_tokens = completion_tokens,
        total_tokens = total_tokens,
        tokens_per_sec = format!("{:.1}", tokens_per_sec),
        "Inference completed"
    );
}

/// Log embedding generation with batch details.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `batch_size` - Number of texts in the batch
/// * `duration_ms` - Time taken in milliseconds
/// * `embedding_dim` - Dimension of the embeddings
pub fn log_embedding_complete(
    model_id: &str,
    batch_size: usize,
    duration_ms: u64,
    embedding_dim: usize,
) {
    let texts_per_sec =
        if duration_ms > 0 { (batch_size as f64 / duration_ms as f64) * 1000.0 } else { 0.0 };

    info!(
        model_id = model_id,
        batch_size = batch_size,
        duration_ms = duration_ms,
        embedding_dim = embedding_dim,
        texts_per_sec = format!("{:.1}", texts_per_sec),
        "Embedding generation completed"
    );
}

/// Log image generation with parameters.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `width` - Image width
/// * `height` - Image height
/// * `duration_ms` - Time taken in milliseconds
pub fn log_image_generation_complete(model_id: &str, width: u32, height: u32, duration_ms: u64) {
    info!(
        model_id = model_id,
        width = width,
        height = height,
        duration_ms = duration_ms,
        "Image generation completed"
    );
}

/// Log speech generation with audio details.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `text_length` - Length of input text
/// * `duration_ms` - Time taken in milliseconds
/// * `audio_duration_secs` - Duration of generated audio in seconds
pub fn log_speech_generation_complete(
    model_id: &str,
    text_length: usize,
    duration_ms: u64,
    audio_duration_secs: f32,
) {
    let realtime_factor =
        if duration_ms > 0 { (audio_duration_secs * 1000.0) / duration_ms as f32 } else { 0.0 };

    info!(
        model_id = model_id,
        text_length = text_length,
        duration_ms = duration_ms,
        audio_duration_secs = format!("{:.2}", audio_duration_secs),
        realtime_factor = format!("{:.2}x", realtime_factor),
        "Speech generation completed"
    );
}

/// Log adapter swap operation.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `from_adapter` - Previous adapter name (if any)
/// * `to_adapter` - New adapter name
/// * `duration_ms` - Time taken in milliseconds
pub fn log_adapter_swap(
    model_id: &str,
    from_adapter: Option<&str>,
    to_adapter: &str,
    duration_ms: u64,
) {
    info!(
        model_id = model_id,
        from_adapter = from_adapter.unwrap_or("none"),
        to_adapter = to_adapter,
        duration_ms = duration_ms,
        "Adapter swapped"
    );
}

/// Log configuration details at debug level.
///
/// # Arguments
///
/// * `model_id` - The model identifier
/// * `config_summary` - Summary of configuration options
pub fn log_config_details(model_id: &str, config_summary: &str) {
    debug!(model_id = model_id, config = config_summary, "Configuration applied");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_guard_elapsed() {
        let guard = TimingGuard::new("test_operation", "test_context");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(guard.elapsed_ms() >= 10);
    }

    #[test]
    fn test_log_functions_dont_panic() {
        // These should not panic even without a subscriber
        log_model_loading_start("test/model", "HuggingFace", true, false);
        log_model_loading_complete("test/model", 1000);
        log_inference_start("test/model", 5, true);
        log_inference_complete("test/model", 500, 100, 50);
        log_embedding_complete("test/model", 10, 200, 768);
        log_image_generation_complete("test/model", 512, 512, 5000);
        log_speech_generation_complete("test/model", 100, 2000, 3.5);
        log_adapter_swap("test/model", Some("adapter1"), "adapter2", 100);
        log_config_details("test/model", "isq=Q4K, paged_attn=true");
    }
}
