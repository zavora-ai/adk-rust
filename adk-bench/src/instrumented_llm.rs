//! Instrumented LLM wrapper for capturing per-call timing metrics.
//!
//! Wraps any `Llm` implementation and forces deterministic configuration
//! (temperature=0, top_p=1.0, seed=42) while recording precise timing
//! for each `generate_content` call.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_bench::InstrumentedLlm;
//!
//! let inner: Arc<dyn Llm> = /* your model */;
//! let instrumented = InstrumentedLlm::new(inner);
//! // Use instrumented as any Llm implementation
//! let records = instrumented.records().await;
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{GenerateContentConfig, Llm, LlmRequest, LlmResponseStream};
use async_trait::async_trait;
use tokio::sync::Mutex;

/// Per-call timing record captured by the instrumented LLM.
#[derive(Debug, Clone)]
pub struct LlmCallRecord {
    /// Monotonic timestamp when the request was sent.
    pub request_sent: Instant,
    /// Monotonic timestamp when the response was fully received.
    pub response_complete: Instant,
    /// Total round-trip duration (request_sent → response_complete).
    pub round_trip: Duration,
    /// Model name used for the call.
    pub model: String,
    /// Prompt token count (if reported by provider).
    pub prompt_tokens: Option<u64>,
    /// Completion token count (if reported by provider).
    pub completion_tokens: Option<u64>,
}

/// Configuration forced on all LLM calls for deterministic output.
///
/// Ensures reproducible benchmarking by overriding sampling parameters
/// on every request sent through the [`InstrumentedLlm`] wrapper.
#[derive(Debug, Clone)]
pub struct DeterministicConfig {
    /// Sampling temperature (0.0 = fully deterministic).
    pub temperature: f32,
    /// Nucleus sampling threshold.
    pub top_p: f32,
    /// Random seed for reproducible generation.
    pub seed: Option<i64>,
}

impl Default for DeterministicConfig {
    fn default() -> Self {
        Self { temperature: 0.0, top_p: 1.0, seed: Some(42) }
    }
}

/// An LLM wrapper that captures per-call timing metrics.
///
/// Wraps any [`Llm`] implementation, forwarding calls while recording
/// precise timestamps for framework overhead calculation. Forces
/// deterministic configuration on every request to ensure reproducible
/// benchmark results.
pub struct InstrumentedLlm {
    inner: Arc<dyn Llm>,
    records: Arc<Mutex<Vec<LlmCallRecord>>>,
    deterministic_config: DeterministicConfig,
}

impl InstrumentedLlm {
    /// Creates a new `InstrumentedLlm` wrapping the given LLM with default
    /// deterministic config (temperature=0.0, top_p=1.0, seed=42).
    pub fn new(inner: Arc<dyn Llm>) -> Self {
        Self {
            inner,
            records: Arc::new(Mutex::new(Vec::new())),
            deterministic_config: DeterministicConfig::default(),
        }
    }

    /// Sets a custom deterministic configuration, consuming and returning self.
    pub fn with_config(mut self, config: DeterministicConfig) -> Self {
        self.deterministic_config = config;
        self
    }

    /// Returns all recorded call timings.
    pub async fn records(&self) -> Vec<LlmCallRecord> {
        self.records.lock().await.clone()
    }

    /// Clears all recorded call timings.
    pub async fn reset(&self) {
        self.records.lock().await.clear();
    }
}

#[async_trait]
impl Llm for InstrumentedLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        mut req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        // Force deterministic config on the request
        let config = req.config.get_or_insert_with(GenerateContentConfig::default);
        config.temperature = Some(self.deterministic_config.temperature);
        config.top_p = Some(self.deterministic_config.top_p);
        if let Some(seed) = self.deterministic_config.seed {
            config.seed = Some(seed);
        }

        // Record start timestamp
        let request_sent = Instant::now();

        // Forward to inner LLM
        let result = self.inner.generate_content(req, stream).await;

        // Record end timestamp
        let response_complete = Instant::now();

        // Create and store the call record
        let record = LlmCallRecord {
            request_sent,
            response_complete,
            round_trip: response_complete.duration_since(request_sent),
            model: self.inner.name().to_string(),
            prompt_tokens: None,
            completion_tokens: None,
        };

        self.records.lock().await.push(record);

        result
    }

    fn schema_adapter(&self) -> &dyn adk_core::schema_adapter::SchemaAdapter {
        self.inner.schema_adapter()
    }
}
