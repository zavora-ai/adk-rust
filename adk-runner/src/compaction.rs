//! Context compaction strategies for managing token budget overflow.
//!
//! This module provides the [`CompactionStrategy`] trait and two built-in
//! implementations for reducing context size when the conversation history
//! exceeds a model's token limit:
//!
//! - [`TruncationCompaction`] — drops oldest events while preserving the system
//!   prompt (first event) and the most recent N events.
//! - [`SummarisationCompaction`] — uses an LLM to summarize older events into a
//!   single condensed event, preserving recent context verbatim.
//!
//! # Overview
//!
//! When a model rejects a request due to token limits, the runner can apply a
//! [`CompactionStrategy`] to shrink the event history and retry. The
//! [`ContextOverflowError`] captures the current token count and the model's
//! maximum, enabling callers to diagnose overflow conditions.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_runner::compaction::{TruncationCompaction, CompactionStrategy};
//! use adk_core::Event;
//!
//! let strategy = TruncationCompaction { preserve_recent: 5 };
//! let events: Vec<Event> = get_conversation_history();
//! let budget = 4096;
//!
//! let compacted = strategy.compact(events, budget).await?;
//! assert!(compacted.len() <= 6); // system prompt + 5 recent
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Event, Llm};
use async_trait::async_trait;
use std::sync::Arc;

/// Raised when the context exceeds the model's token limit.
///
/// This error captures both the current token count and the model's maximum
/// token limit, enabling callers to understand the overflow magnitude and
/// decide on a compaction strategy.
///
/// # Example
///
/// ```rust
/// use adk_runner::compaction::ContextOverflowError;
///
/// let err = ContextOverflowError {
///     token_count: 128_000,
///     limit: 100_000,
/// };
/// assert_eq!(
///     err.to_string(),
///     "Context overflow: 128000 tokens (limit: 100000)"
/// );
/// ```
#[derive(Debug, thiserror::Error)]
#[error("Context overflow: {token_count} tokens (limit: {limit})")]
pub struct ContextOverflowError {
    /// The current estimated token count of the context.
    pub token_count: usize,
    /// The model's maximum token limit.
    pub limit: usize,
}

impl From<ContextOverflowError> for AdkError {
    fn from(err: ContextOverflowError) -> Self {
        AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::InvalidInput,
            "runner.context_overflow",
            err.to_string(),
        )
    }
}

/// Strategy for reducing context size when token limits are exceeded.
///
/// Implementations receive the full event history and a token budget, and must
/// return a compacted event list that fits within the budget. The compacted
/// list should preserve semantic coherence — at minimum the system prompt and
/// recent interactions.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::compaction::CompactionStrategy;
/// use adk_core::Event;
///
/// struct MyStrategy;
///
/// #[async_trait::async_trait]
/// impl CompactionStrategy for MyStrategy {
///     async fn compact(
///         &self,
///         events: Vec<Event>,
///         budget: usize,
///     ) -> Result<Vec<Event>, adk_core::AdkError> {
///         // Custom compaction logic
///         Ok(events)
///     }
/// }
/// ```
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Compact the event history to fit within the token budget.
    ///
    /// # Arguments
    ///
    /// * `events` — the full conversation event history
    /// * `budget` — the target maximum token count after compaction
    ///
    /// # Errors
    ///
    /// Returns an error if compaction fails (e.g., summarization LLM call fails).
    async fn compact(&self, events: Vec<Event>, budget: usize) -> Result<Vec<Event>, AdkError>;
}

/// Drops oldest events while preserving the system prompt and most recent N events.
///
/// This is the simplest compaction strategy: it keeps the first event (assumed to
/// be the system prompt) and the last `preserve_recent` events, discarding
/// everything in between. No LLM calls are made.
///
/// # Fields
///
/// * `preserve_recent` — number of most-recent events to keep (in addition to
///   the system prompt).
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::compaction::{TruncationCompaction, CompactionStrategy};
/// use adk_core::Event;
///
/// let strategy = TruncationCompaction { preserve_recent: 3 };
///
/// // Given 10 events, keeps event[0] (system prompt) + events[7..10]
/// let events = vec![Event::new("inv"); 10];
/// let compacted = strategy.compact(events, 4096).await?;
/// assert_eq!(compacted.len(), 4); // 1 system + 3 recent
/// ```
pub struct TruncationCompaction {
    /// Number of most-recent events to preserve (excluding the system prompt).
    pub preserve_recent: usize,
}

#[async_trait]
impl CompactionStrategy for TruncationCompaction {
    async fn compact(&self, events: Vec<Event>, _budget: usize) -> Result<Vec<Event>, AdkError> {
        let len = events.len();

        // Nothing to compact if we have fewer events than what we'd preserve
        if len <= self.preserve_recent + 1 {
            return Ok(events);
        }

        let mut compacted = Vec::with_capacity(self.preserve_recent + 1);

        // Preserve the first event (system prompt)
        if let Some(first) = events.first() {
            compacted.push(first.clone());
        }

        // Preserve the last N events
        let start = len.saturating_sub(self.preserve_recent);
        compacted.extend_from_slice(&events[start..]);

        tracing::debug!(
            original_count = len,
            compacted_count = compacted.len(),
            dropped = len - compacted.len(),
            "truncation compaction applied"
        );

        Ok(compacted)
    }
}

/// Summarizes older events into a single condensed event using an LLM.
///
/// This strategy takes the oldest events (excluding the most recent
/// `turns_to_summarise` boundary), sends them to an LLM with a summarization
/// prompt, and replaces them with a single summary event. Recent events are
/// preserved verbatim to maintain conversational continuity.
///
/// The summary call goes through [`adk_core::generate_metered`], so when the
/// runner compacts a session it counts against the run's budget and spend
/// ledger, and a call the budget refuses is never sent.
///
/// # Fields
///
/// * `model` — the LLM used to generate summaries
/// * `turns_to_summarise` — number of oldest turns to summarize (the rest are
///   kept as-is)
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::compaction::{SummarisationCompaction, CompactionStrategy};
/// use adk_core::Event;
/// use std::sync::Arc;
///
/// let model: Arc<dyn adk_core::Llm> = get_summarization_model();
/// let strategy = SummarisationCompaction {
///     model,
///     turns_to_summarise: 10,
/// };
///
/// let events = get_long_conversation();
/// let compacted = strategy.compact(events, 4096).await?;
/// // First event is now a summary, followed by recent events
/// ```
pub struct SummarisationCompaction {
    /// The LLM used to generate summaries of older events.
    pub model: Arc<dyn Llm>,
    /// Number of oldest turns to summarize into a single event.
    pub turns_to_summarise: usize,
}

#[async_trait]
impl CompactionStrategy for SummarisationCompaction {
    async fn compact(&self, events: Vec<Event>, _budget: usize) -> Result<Vec<Event>, AdkError> {
        let len = events.len();

        // If we don't have enough events to summarize, return as-is
        if len <= self.turns_to_summarise {
            return Ok(events);
        }

        // Split: events to summarize vs. events to preserve
        let summarize_end = self.turns_to_summarise.min(len);
        let events_to_summarize = &events[..summarize_end];
        let events_to_preserve = &events[summarize_end..];

        // Build a summarization prompt from the events to compress
        let summary_text = build_summary_prompt(events_to_summarize);

        let request = adk_core::LlmRequest::new(
            self.model.name().to_string(),
            vec![adk_core::Content::new("user").with_text(summary_text)],
        );

        // Counts against the run's budget and spend ledger when the runner made a scope current.
        let mut stream = adk_core::generate_metered(self.model.as_ref(), request, false).await?;

        // Collect the full response
        use futures::StreamExt;
        let mut summary_content = String::new();
        while let Some(response) = stream.next().await {
            let response = response?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let adk_core::Part::Text { text } = part {
                        summary_content.push_str(text);
                    }
                }
            }
        }

        // Create a summary event
        let mut summary_event = Event::new("compaction");
        summary_event.author = "system".to_string();
        summary_event.set_content(
            adk_core::Content::new("model")
                .with_text(format!("[Context Summary]\n{summary_content}")),
        );

        // Build the compacted list: summary + preserved events
        let mut compacted = Vec::with_capacity(1 + events_to_preserve.len());
        compacted.push(summary_event);
        compacted.extend_from_slice(events_to_preserve);

        tracing::debug!(
            original_count = len,
            summarized_count = summarize_end,
            preserved_count = events_to_preserve.len(),
            "summarisation compaction applied"
        );

        Ok(compacted)
    }
}

/// Builds a summarization prompt from a slice of events.
fn build_summary_prompt(events: &[Event]) -> String {
    let mut prompt = String::from(
        "Summarize the following conversation history into a concise summary \
         that preserves key facts, decisions, and context. Be brief but complete.\n\n",
    );

    for event in events {
        if let Some(content) = event.content() {
            prompt.push_str(&format!("[{}]: ", content.role));
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    prompt.push_str(text);
                }
            }
            prompt.push('\n');
        }
    }

    prompt
}

/// Configuration for automatic context compaction in the runner.
///
/// When the conversation history exceeds `context_budget` tokens, the runner
/// applies the configured [`CompactionStrategy`] to shrink the event list. If
/// compaction still leaves the context over budget, the runner retries up to
/// `max_retries` times before returning a [`ContextOverflowError`].
///
/// # Fields
///
/// * `strategy` — the active compaction strategy (e.g., truncation or summarisation)
/// * `context_budget` — maximum token count before triggering compaction
/// * `max_retries` — maximum number of compaction-retry cycles (default: 2)
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::compaction::{CompactionConfig, TruncationCompaction};
///
/// let config = CompactionConfig {
///     strategy: Box::new(TruncationCompaction { preserve_recent: 10 }),
///     context_budget: 100_000,
///     max_retries: 2,
/// };
///
/// // Use in RunConfig:
/// // run_config.compaction = Some(config);
/// ```
pub struct CompactionConfig {
    /// The active compaction strategy used to reduce context size.
    pub strategy: Box<dyn CompactionStrategy>,
    /// Maximum token count before triggering compaction.
    pub context_budget: usize,
    /// Maximum number of compaction-retry cycles.
    ///
    /// If compaction does not bring the context under budget after this many
    /// attempts, a [`ContextOverflowError`] is returned. Defaults to `2`.
    pub max_retries: usize,
}

impl CompactionConfig {
    /// Creates a new `CompactionConfig` with the given strategy and budget.
    ///
    /// Uses the default `max_retries` value of `2`.
    ///
    /// # Arguments
    ///
    /// * `strategy` — the compaction strategy to apply
    /// * `context_budget` — the token threshold that triggers compaction
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_runner::compaction::{CompactionConfig, TruncationCompaction};
    ///
    /// let config = CompactionConfig::new(
    ///     Box::new(TruncationCompaction { preserve_recent: 5 }),
    ///     50_000,
    /// );
    /// assert_eq!(config.max_retries, 2);
    /// ```
    pub fn new(strategy: Box<dyn CompactionStrategy>, context_budget: usize) -> Self {
        Self { strategy, context_budget, max_retries: 2 }
    }
}

impl std::fmt::Debug for CompactionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactionConfig")
            .field("context_budget", &self.context_budget)
            .field("max_retries", &self.max_retries)
            .field("strategy", &"<dyn CompactionStrategy>")
            .finish()
    }
}

/// Checks whether an [`AdkError`] represents a token/context limit error from a model.
///
/// Token limit errors are typically reported by model providers as
/// `ErrorCategory::InvalidInput` with messages containing keywords like
/// "token", "context length", "too long", or "maximum context". This function
/// uses heuristic message matching since different providers use different
/// phrasing.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorComponent, ErrorCategory};
/// use adk_runner::compaction::is_token_limit_error;
///
/// let err = AdkError::new(
///     ErrorComponent::Model,
///     ErrorCategory::InvalidInput,
///     "model.openai.bad_request",
///     "This model's maximum context length is 128000 tokens",
/// );
/// assert!(is_token_limit_error(&err));
///
/// let other = AdkError::model("connection refused");
/// assert!(!is_token_limit_error(&other));
/// ```
pub fn is_token_limit_error(err: &AdkError) -> bool {
    // Must be a model error with InvalidInput category
    if err.component != adk_core::ErrorComponent::Model {
        return false;
    }
    if err.category != adk_core::ErrorCategory::InvalidInput {
        return false;
    }

    // Check for the specific context overflow code we emit
    if err.code == "runner.context_overflow" {
        return true;
    }

    // Heuristic: check the error message for token/context limit keywords.
    // Different providers phrase this differently:
    // - OpenAI: "maximum context length is X tokens"
    // - Anthropic: "prompt is too long: X tokens"
    // - Gemini: "request payload size exceeds the limit"
    // - Generic: "token limit", "context length exceeded"
    let msg = err.message.to_lowercase();
    let token_limit_patterns = [
        "token",
        "context length",
        "context_length",
        "too long",
        "too many tokens",
        "payload size exceeds",
        "maximum context",
        "max_tokens",
        "input too large",
        "prompt is too long",
        "exceeds the model",
    ];

    token_limit_patterns.iter().any(|pattern| msg.contains(pattern))
}

/// Applies compaction to the given events and returns the compacted result.
///
/// This is the core retry loop used by the runner when a token limit error is
/// detected. It applies the configured [`CompactionStrategy`] up to
/// `max_retries` times. If compaction cannot reduce the context below the
/// budget after all retries, a [`ContextOverflowError`] is returned.
///
/// # Arguments
///
/// * `config` — the compaction configuration (strategy, budget, max_retries)
/// * `events` — the current event history to compact
///
/// # Returns
///
/// The compacted event list on success, or a [`ContextOverflowError`] wrapped
/// in [`AdkError`] if compaction fails to bring the context under budget.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::compaction::{apply_compaction_with_retry, CompactionConfig, TruncationCompaction};
///
/// let config = CompactionConfig::new(
///     Box::new(TruncationCompaction { preserve_recent: 5 }),
///     4096,
/// );
/// let events = get_session_events();
/// let compacted = apply_compaction_with_retry(&config, events).await?;
/// ```
pub async fn apply_compaction_with_retry(
    config: &CompactionConfig,
    events: Vec<Event>,
) -> Result<Vec<Event>, AdkError> {
    let mut current_events = events;

    for attempt in 0..config.max_retries {
        tracing::info!(
            attempt = attempt + 1,
            max_retries = config.max_retries,
            event_count = current_events.len(),
            budget = config.context_budget,
            "applying context compaction"
        );

        current_events = config.strategy.compact(current_events, config.context_budget).await?;

        // Estimate token count after compaction using a simple heuristic:
        // ~4 chars per token (same as adk-core's estimate_tokens).
        let estimated_tokens = estimate_event_tokens(&current_events);

        if estimated_tokens <= config.context_budget {
            tracing::info!(
                estimated_tokens,
                budget = config.context_budget,
                "compaction succeeded, context within budget"
            );
            return Ok(current_events);
        }

        tracing::warn!(
            estimated_tokens,
            budget = config.context_budget,
            attempt = attempt + 1,
            "compaction did not bring context under budget, retrying"
        );
    }

    // All retries exhausted — return ContextOverflowError
    let final_tokens = estimate_event_tokens(&current_events);
    Err(ContextOverflowError { token_count: final_tokens, limit: config.context_budget }.into())
}

/// Estimates the total token count for a slice of events.
///
/// Uses a simple heuristic of ~4 characters per token, consistent with
/// `adk_core::intra_compaction::estimate_tokens`.
pub fn estimate_event_tokens(events: &[Event]) -> usize {
    let total_chars: usize = events
        .iter()
        .map(|e| {
            e.content()
                .map(|c| {
                    c.parts
                        .iter()
                        .map(|p| match p {
                            adk_core::Part::Text { text } => text.len(),
                            _ => 20, // rough estimate for non-text parts
                        })
                        .sum::<usize>()
                })
                .unwrap_or(0)
        })
        .sum();

    // ~4 chars per token
    total_chars / 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, Event};

    fn make_events(count: usize) -> Vec<Event> {
        (0..count)
            .map(|i| {
                let mut event = Event::new("test-inv");
                event.author = if i == 0 { "system".to_string() } else { "user".to_string() };
                event.set_content(Content::new("user").with_text(format!("message {i}")));
                event
            })
            .collect()
    }

    struct PricedSummaryModel;

    #[async_trait]
    impl Llm for PricedSummaryModel {
        fn name(&self) -> &str {
            "gemini-3.5-flash-lite"
        }

        async fn generate_content(
            &self,
            _request: adk_core::LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            let response = adk_core::LlmResponse {
                content: Some(Content::new("model").with_text("summary")),
                usage_metadata: Some(adk_core::UsageMetadata {
                    total_token_count: 50,
                    cost: Some(0.0005),
                    ..Default::default()
                }),
                provider: Some("gemini".to_string()),
                turn_complete: true,
                ..Default::default()
            };
            Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
        }
    }

    #[tokio::test]
    async fn summarisation_counts_against_the_current_scope_and_stops_over_budget() {
        use adk_core::{BudgetTracker, ModelCallScope, RunBudget};

        let strategy =
            SummarisationCompaction { model: Arc::new(PricedSummaryModel), turns_to_summarise: 4 };
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
        let scope = ModelCallScope::new().with_budget(tracker.clone());

        let compacted = scope.clone().within(strategy.compact(make_events(10), 4096)).await;
        assert_eq!(compacted.unwrap().len(), 7);
        assert_eq!(tracker.usage().total_tokens, 50);
        assert_eq!(tracker.usage().cost_micro_usd, 500);

        let refused = scope.within(strategy.compact(make_events(10), 4096)).await;
        assert_eq!(refused.unwrap_err().code, "budget.model_calls");
        assert_eq!(tracker.usage().model_calls, 1);
    }

    #[tokio::test]
    async fn test_truncation_preserves_system_and_recent() {
        let strategy = TruncationCompaction { preserve_recent: 3 };
        let events = make_events(10);

        let compacted = strategy.compact(events.clone(), 4096).await.unwrap();

        // Should have 1 (system) + 3 (recent) = 4 events
        assert_eq!(compacted.len(), 4);
        // First event is the system prompt
        assert_eq!(compacted[0].author, "system");
        // Last 3 are the most recent
        assert_eq!(compacted[1].id, events[7].id);
        assert_eq!(compacted[2].id, events[8].id);
        assert_eq!(compacted[3].id, events[9].id);
    }

    #[tokio::test]
    async fn test_truncation_no_op_when_few_events() {
        let strategy = TruncationCompaction { preserve_recent: 5 };
        let events = make_events(3);

        let compacted = strategy.compact(events.clone(), 4096).await.unwrap();

        // Should return all events unchanged
        assert_eq!(compacted.len(), 3);
    }

    #[tokio::test]
    async fn test_truncation_exact_boundary() {
        let strategy = TruncationCompaction { preserve_recent: 4 };
        let events = make_events(5); // 1 system + 4 recent = exactly the preserve count

        let compacted = strategy.compact(events.clone(), 4096).await.unwrap();

        // 5 events with preserve_recent=4 means we keep all (1+4=5)
        assert_eq!(compacted.len(), 5);
    }

    #[test]
    fn test_context_overflow_error_display() {
        let err = ContextOverflowError { token_count: 50_000, limit: 32_000 };
        assert_eq!(err.to_string(), "Context overflow: 50000 tokens (limit: 32000)");
    }

    #[test]
    fn test_context_overflow_error_into_adk_error() {
        let err = ContextOverflowError { token_count: 50_000, limit: 32_000 };
        let adk_err: AdkError = err.into();
        assert!(adk_err.is_model());
        assert_eq!(adk_err.code, "runner.context_overflow");
    }

    #[test]
    fn test_compaction_config_new_defaults_max_retries() {
        let strategy = TruncationCompaction { preserve_recent: 5 };
        let config = CompactionConfig::new(Box::new(strategy), 100_000);

        assert_eq!(config.context_budget, 100_000);
        assert_eq!(config.max_retries, 2);
    }

    #[test]
    fn test_compaction_config_custom_max_retries() {
        let config = CompactionConfig {
            strategy: Box::new(TruncationCompaction { preserve_recent: 3 }),
            context_budget: 50_000,
            max_retries: 5,
        };

        assert_eq!(config.context_budget, 50_000);
        assert_eq!(config.max_retries, 5);
    }

    #[test]
    fn test_compaction_config_debug() {
        let config =
            CompactionConfig::new(Box::new(TruncationCompaction { preserve_recent: 3 }), 32_000);
        let debug_str = format!("{config:?}");
        assert!(debug_str.contains("CompactionConfig"));
        assert!(debug_str.contains("32000"));
        assert!(debug_str.contains("max_retries: 2"));
    }

    #[test]
    fn test_build_summary_prompt() {
        let mut events = Vec::new();
        let mut e1 = Event::new("inv");
        e1.set_content(Content::new("user").with_text("Hello"));
        events.push(e1);

        let mut e2 = Event::new("inv");
        e2.set_content(Content::new("model").with_text("Hi there!"));
        events.push(e2);

        let prompt = build_summary_prompt(&events);
        assert!(prompt.contains("[user]: Hello"));
        assert!(prompt.contains("[model]: Hi there!"));
        assert!(prompt.contains("Summarize"));
    }

    #[test]
    fn test_is_token_limit_error_detects_openai_style() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "model.openai.bad_request",
            "This model's maximum context length is 128000 tokens",
        );
        assert!(is_token_limit_error(&err));
    }

    #[test]
    fn test_is_token_limit_error_detects_anthropic_style() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "model.anthropic.bad_request",
            "prompt is too long: 200000 tokens > 100000 maximum",
        );
        assert!(is_token_limit_error(&err));
    }

    #[test]
    fn test_is_token_limit_error_detects_context_overflow_code() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "runner.context_overflow",
            "Context overflow: 50000 tokens (limit: 32000)",
        );
        assert!(is_token_limit_error(&err));
    }

    #[test]
    fn test_is_token_limit_error_rejects_non_model_error() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Tool,
            adk_core::ErrorCategory::InvalidInput,
            "tool.error",
            "token limit exceeded",
        );
        assert!(!is_token_limit_error(&err));
    }

    #[test]
    fn test_is_token_limit_error_rejects_non_invalid_input() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::Internal,
            "model.internal",
            "token limit exceeded",
        );
        assert!(!is_token_limit_error(&err));
    }

    #[test]
    fn test_is_token_limit_error_rejects_unrelated_invalid_input() {
        let err = AdkError::new(
            adk_core::ErrorComponent::Model,
            adk_core::ErrorCategory::InvalidInput,
            "model.openai.bad_request",
            "invalid JSON in request body",
        );
        assert!(!is_token_limit_error(&err));
    }

    #[test]
    fn test_estimate_event_tokens_empty() {
        let events: Vec<Event> = Vec::new();
        assert_eq!(estimate_event_tokens(&events), 0);
    }

    #[test]
    fn test_estimate_event_tokens_with_content() {
        // "Hello world" = 11 chars, ~2-3 tokens at 4 chars/token
        let mut event = Event::new("inv");
        event.set_content(Content::new("user").with_text("Hello world"));
        let events = vec![event];
        assert_eq!(estimate_event_tokens(&events), 11 / 4); // 2
    }

    #[tokio::test]
    async fn test_apply_compaction_with_retry_succeeds_first_try() {
        let strategy = TruncationCompaction { preserve_recent: 2 };
        let config = CompactionConfig {
            strategy: Box::new(strategy),
            context_budget: 100, // generous budget
            max_retries: 2,
        };

        let events = make_events(10);
        let result = apply_compaction_with_retry(&config, events).await;
        assert!(result.is_ok());
        let compacted = result.unwrap();
        // TruncationCompaction keeps 1 system + 2 recent = 3
        assert_eq!(compacted.len(), 3);
    }

    #[tokio::test]
    async fn test_apply_compaction_with_retry_fails_when_budget_too_small() {
        let strategy = TruncationCompaction { preserve_recent: 2 };
        let config = CompactionConfig {
            strategy: Box::new(strategy),
            context_budget: 0, // impossible budget
            max_retries: 2,
        };

        // Create events with enough content to exceed budget=0
        let mut events = Vec::new();
        for i in 0..5 {
            let mut e = Event::new("inv");
            e.set_content(Content::new("user").with_text(format!("message {i} with some content")));
            events.push(e);
        }

        let result = apply_compaction_with_retry(&config, events).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "runner.context_overflow");
    }
}
