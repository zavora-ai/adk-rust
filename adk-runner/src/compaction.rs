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
/// everything in between. No LLM calls are made. When the first kept event would be a
/// function response whose call is dropped, the call and the events after it are kept
/// too ([`adk_core::history::balanced_cut`]), since providers reject a response without
/// its call.
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

        // Preserve the last N events, and more when the Nth from last is a function response
        // whose call would otherwise be dropped.
        let start = adk_core::history::balanced_cut(&events, len - self.preserve_recent);
        if start <= 1 {
            return Ok(events);
        }

        let mut compacted = Vec::with_capacity(len - start + 1);

        // Preserve the first event (system prompt)
        if let Some(first) = events.first() {
            compacted.push(first.clone());
        }
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
/// The prompt renders each event's text, function calls and responses, and data
/// references ([`adk_core::intra_compaction::render_part_for_summary`]), so the summary
/// keeps what tools returned. The split never separates a function response from its call
/// ([`adk_core::history::balanced_cut`]). The events stay as they are when the summary is
/// empty or not smaller than they are; a stream error, or a model that stops for any reason
/// but `Stop`, fails the compaction with `model.summary_incomplete`. For a stream error that
/// error keeps the stream error's category, retry hint, and details, records its code as
/// `details.metadata["cause_code"]`, and has it as its
/// [`source`](std::error::Error::source).
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

        // Split: events to summarize vs. events to preserve. The split never separates a
        // function response from its call.
        let summarize_end = adk_core::history::balanced_cut(&events, self.turns_to_summarise);
        if summarize_end == 0 {
            return Ok(events);
        }
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
        let mut finish_reason = None;
        while let Some(response) = stream.next().await {
            let response = response.map_err(|cause| {
                let mut details = (*cause.details).clone();
                details.metadata.insert("cause_code".to_string(), cause.code.into());
                AdkError::new(
                    ErrorComponent::Model,
                    cause.category,
                    "model.summary_incomplete",
                    format!(
                        "the summarization model failed before finishing its summary ({}); \
                         summarise fewer turns or retry",
                        cause.message
                    ),
                )
                .with_retry(cause.retry.clone())
                .with_details(details)
                .with_source(cause)
            })?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let adk_core::Part::Text { text } = part {
                        summary_content.push_str(text);
                    }
                }
            }
            finish_reason = response.finish_reason.or(finish_reason);
        }
        // A summary the model cut short would replace history with part of what it said.
        if let Some(
            reason @ (adk_core::FinishReason::MaxTokens
            | adk_core::FinishReason::Safety
            | adk_core::FinishReason::Recitation
            | adk_core::FinishReason::Other),
        ) = finish_reason
        {
            return Err(AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Internal,
                "model.summary_incomplete",
                format!(
                    "the summarization model stopped before finishing its summary ({reason:?}); \
                     raise its output limit or summarise fewer turns"
                ),
            ));
        }

        // Create a summary event
        let mut summary_event = Event::new("compaction");
        summary_event.author = "system".to_string();
        summary_event.set_content(
            adk_core::Content::new("model")
                .with_text(format!("[Context Summary]\n{summary_content}")),
        );
        // A summary that is not smaller than what it replaces would not shrink the request.
        if summary_content.trim().is_empty()
            || estimate_event_tokens(std::slice::from_ref(&summary_event))
                >= estimate_event_tokens(events_to_summarize)
        {
            tracing::info!("summary is not smaller than the events it replaces; keeping them");
            return Ok(events);
        }

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
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = event.content() {
            for rendered in
                content.parts.iter().filter_map(adk_core::intra_compaction::render_part_for_summary)
            {
                prompt.push_str(&format!("[{}]: {rendered}\n", content.role));
            }
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
/// The strategy is given only the events before the earliest call still held for
/// confirmation ([`adk_core::held_call_compaction_limit`]); that call and every later
/// event are kept as they are, so an approval given later still resumes the call.
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

        // The strategy sees only the events before the earliest held call; that call and
        // everything after it stay, so its approval can still resume it.
        let held_from = adk_core::history::balanced_cut(
            &current_events,
            adk_core::held_call_compaction_limit(&current_events, current_events.len()),
        );
        let kept = current_events.split_off(held_from);
        current_events = config.strategy.compact(current_events, config.context_budget).await?;
        current_events.extend(kept);

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
/// Uses [`adk_core::intra_compaction::estimate_tokens`] at ~4 characters per token, which
/// counts function call arguments and responses as well as text, so a history made of large
/// tool results crosses the budget.
pub fn estimate_event_tokens(events: &[Event]) -> usize {
    adk_core::intra_compaction::estimate_tokens(events, 4) as usize
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

    /// `before` user messages, then a `transfer` call `ops` holds for confirmation (call,
    /// pending response, request), then `after` user messages, one millisecond apart.
    fn with_held_call(before: usize, after: usize) -> Vec<Event> {
        let mut events = make_events(before);
        let mut call = Event::new("inv-held");
        call.author = "ops".to_string();
        call.set_content(Content {
            role: "model".to_string(),
            parts: vec![adk_core::Part::FunctionCall {
                name: "transfer".to_string(),
                args: serde_json::json!({ "amount": 10 }),
                id: Some("c1".to_string()),
                thought_signature: None,
            }],
        });
        let mut pending = Event::new("inv-held");
        pending.author = "ops".to_string();
        pending.set_content(Content {
            role: "function".to_string(),
            parts: vec![adk_core::Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new(
                    "transfer",
                    adk_core::confirmation_pending_response("held"),
                ),
                id: Some("c1".to_string()),
                annotations: None,
            }],
        });
        let mut hold = Event::new("inv-held");
        hold.author = "ops".to_string();
        hold.actions.tool_confirmation = Some(adk_core::ToolConfirmationRequest {
            tool_name: "transfer".to_string(),
            function_call_id: Some("c1".to_string()),
            args: serde_json::json!({ "amount": 10 }),
            acting: None,
            origin: None,
        });
        events.extend([call, pending, hold]);
        events.extend(make_events(after));
        let start = chrono::Utc::now();
        for (index, event) in events.iter_mut().enumerate() {
            event.timestamp = start + chrono::Duration::milliseconds(index as i64);
        }
        events
    }

    #[tokio::test]
    async fn context_compaction_keeps_a_held_call_and_everything_after_it() {
        let events = with_held_call(8, 3);
        let config = CompactionConfig::new(
            Box::new(TruncationCompaction { preserve_recent: 1 }),
            usize::MAX,
        );
        let compacted = apply_compaction_with_retry(&config, events.clone()).await.unwrap();

        let ids =
            |events: &[Event]| events.iter().map(|event| event.id.clone()).collect::<Vec<_>>();
        // Truncation kept the first and last of the 8 events before the held call.
        assert_eq!(ids(&compacted[..2]), ids(&[events[0].clone(), events[7].clone()]));
        assert_eq!(ids(&compacted[2..]), ids(&events[8..]), "the held call and later events stay");
        assert_eq!(adk_core::held_tool_calls(&compacted, "ops", "").len(), 1);
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

    fn call_and_result(id: &str, payload: serde_json::Value) -> [Event; 2] {
        let mut call = Event::new("inv");
        call.author = "desk".to_string();
        call.set_content(Content {
            role: "model".to_string(),
            parts: vec![adk_core::Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({"order": "A1"}),
                id: Some(id.to_string()),
                thought_signature: None,
            }],
        });
        let mut result = Event::new("inv");
        result.author = "desk".to_string();
        result.set_content(Content {
            role: "function".to_string(),
            parts: vec![adk_core::Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new("lookup", payload),
                id: Some(id.to_string()),
                annotations: None,
            }],
        });
        [call, result]
    }

    /// A summarizer that answers `text` and stops for `finish`, recording its prompt.
    struct ScriptedSummary {
        text: String,
        finish: adk_core::FinishReason,
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptedSummary {
        fn new(text: &str, finish: adk_core::FinishReason) -> Arc<Self> {
            Arc::new(Self { text: text.to_string(), finish, prompts: Default::default() })
        }
    }

    #[async_trait]
    impl Llm for ScriptedSummary {
        fn name(&self) -> &str {
            "summary"
        }

        async fn generate_content(
            &self,
            request: adk_core::LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            let prompt = request.contents[0].parts[0].text().unwrap_or_default().to_string();
            self.prompts.lock().unwrap().push(prompt);
            let response = adk_core::LlmResponse {
                content: Some(Content::new("model").with_text(&self.text)),
                finish_reason: Some(self.finish),
                turn_complete: true,
                ..Default::default()
            };
            Ok(Box::pin(futures::stream::iter(vec![Ok(response)])))
        }
    }

    #[tokio::test]
    async fn truncation_keeps_the_call_of_a_kept_result() {
        // [q1, a1, q2, call, result, a2, q3]: keeping three would start on the result.
        let mut events = make_events(3);
        events.extend(call_and_result("c1", serde_json::json!({"status": "shipped"})));
        events.extend(make_events(2));
        let compacted = TruncationCompaction { preserve_recent: 3 }
            .compact(events.clone(), 4096)
            .await
            .unwrap();
        let ids = compacted.iter().map(|event| event.id.clone()).collect::<Vec<_>>();
        let expected = [&events[..1], &events[3..]]
            .concat()
            .iter()
            .map(|event| event.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn summarisation_never_splits_a_call_from_its_result_and_keeps_tool_facts() {
        // [q1, call, result, a1, q2]: summarising two would leave the result without its call.
        let mut events = vec![make_events(1)[0].clone()];
        events[0].set_content(Content::new("user").with_text("background ".repeat(50)));
        events.extend(call_and_result("c1", serde_json::json!({"warehouse": "Nairobi-7"})));
        events.extend(make_events(2));
        let model = ScriptedSummary::new("short", adk_core::FinishReason::Stop);
        let compacted = SummarisationCompaction { model: model.clone(), turns_to_summarise: 2 }
            .compact(events.clone(), 4096)
            .await
            .unwrap();
        assert_eq!(compacted.len(), 5, "the summary, then the call, its result and two more");
        assert_eq!(compacted[1].id, events[1].id);

        let summary_of_tools =
            SummarisationCompaction { model: model.clone(), turns_to_summarise: 3 }
                .compact(events.clone(), 4096)
                .await
                .unwrap();
        assert_eq!(summary_of_tools.len(), 3);
        let prompts = model.prompts.lock().unwrap().clone();
        assert!(prompts[1].contains(r#"[function response] lookup: {"warehouse":"Nairobi-7"}"#));
        assert!(prompts[1].contains(r#"[function call] lookup({"order":"A1"})"#));
    }

    #[tokio::test]
    async fn summarisation_keeps_the_events_when_the_summary_does_not_shrink_them() {
        let events = make_events(6);
        let model = ScriptedSummary::new(&"long summary ".repeat(50), adk_core::FinishReason::Stop);
        let compacted = SummarisationCompaction { model, turns_to_summarise: 3 }
            .compact(events.clone(), 4096)
            .await
            .unwrap();
        assert_eq!(compacted.len(), events.len());
    }

    #[tokio::test]
    async fn summarisation_fails_when_the_summary_was_cut_short() {
        let model = ScriptedSummary::new("Summary: the cust", adk_core::FinishReason::MaxTokens);
        let error = SummarisationCompaction { model, turns_to_summarise: 3 }
            .compact(make_events(6), 4096)
            .await
            .unwrap_err();
        assert_eq!(error.code, "model.summary_incomplete");
    }

    struct FailingSummary;

    #[async_trait]
    impl Llm for FailingSummary {
        fn name(&self) -> &str {
            "summary"
        }

        async fn generate_content(
            &self,
            _request: adk_core::LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            let partial = adk_core::LlmResponse {
                content: Some(Content::new("model").with_text("Summary: the cust")),
                partial: true,
                ..Default::default()
            };
            let stalled = AdkError::new(
                ErrorComponent::Model,
                ErrorCategory::Timeout,
                "model.openai_compat.request",
                "the response stopped arriving",
            )
            .with_provider("openai");
            Ok(Box::pin(futures::stream::iter(vec![Ok(partial), Err(stalled)])))
        }
    }

    #[tokio::test]
    async fn summarisation_fails_with_the_cause_when_the_stream_fails() {
        let error =
            SummarisationCompaction { model: Arc::new(FailingSummary), turns_to_summarise: 3 }
                .compact(make_events(6), 4096)
                .await
                .unwrap_err();
        assert_eq!(error.code, "model.summary_incomplete");
        assert_eq!(error.category, ErrorCategory::Timeout);
        assert!(error.is_retryable());
        assert_eq!(error.details.provider.as_deref(), Some("openai"));
        assert_eq!(error.details.metadata["cause_code"], "model.openai_compat.request");
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn the_estimate_counts_tool_payloads() {
        let events = call_and_result("c1", serde_json::json!({"notes": "n".repeat(4000)}));
        assert!(estimate_event_tokens(&events) > 1000, "{}", estimate_event_tokens(&events));
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
