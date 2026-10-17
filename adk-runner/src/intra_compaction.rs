//! Intra-invocation context compaction trigger logic.
//!
//! This module provides [`IntraInvocationCompactor`], which checks whether the
//! estimated token count of conversation events exceeds a threshold and triggers
//! summarization using the existing [`BaseEventsSummarizer`] trait. The actual
//! summarization logic lives in `adk-agent/src/compaction.rs` (`LlmEventSummarizer`);
//! this module only handles the trigger decision and overlap preservation.

use adk_core::intra_compaction::{IntraCompactionConfig, estimate_tokens};
use adk_core::{BaseEventsSummarizer, Event, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Intra-invocation compactor that monitors token usage during a single
/// invocation and triggers summarization when the context exceeds a threshold.
///
/// The compactor enforces at-most-once compaction per LLM call cycle via an
/// [`AtomicBool`] guard. Call [`reset_cycle`](Self::reset_cycle) at the start
/// of each LLM call to re-arm the guard.
///
/// # Example
///
/// ```rust,ignore
/// use adk_runner::IntraInvocationCompactor;
/// use adk_core::IntraCompactionConfig;
///
/// let compactor = IntraInvocationCompactor::new(
///     IntraCompactionConfig::default(),
///     summarizer,
/// );
///
/// // Before each LLM call:
/// compactor.reset_cycle();
/// if let Some(compacted) = compactor.maybe_compact(&events).await? {
///     events = compacted;
/// }
/// ```
pub struct IntraInvocationCompactor {
    config: IntraCompactionConfig,
    /// Reuses the existing `BaseEventsSummarizer` trait from adk-core.
    summarizer: Arc<dyn BaseEventsSummarizer>,
    /// Track whether compaction already ran this LLM call cycle.
    compacted_this_cycle: AtomicBool,
}

impl IntraInvocationCompactor {
    /// Create a new compactor with the given config and summarizer.
    pub fn new(config: IntraCompactionConfig, summarizer: Arc<dyn BaseEventsSummarizer>) -> Self {
        Self { config, summarizer, compacted_this_cycle: AtomicBool::new(false) }
    }

    /// Check if compaction is needed and perform it if so.
    ///
    /// Returns `Some(compacted_events)` if compaction was triggered, or `None`
    /// if no compaction was needed (below threshold or already compacted this cycle).
    ///
    /// On summarizer error, logs a warning and returns `None` (uncompacted history
    /// is used).
    pub async fn maybe_compact(&self, events: &[Event]) -> Result<Option<Vec<Event>>> {
        // Guard: at most once per cycle
        if self
            .compacted_this_cycle
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(None);
        }

        let estimated = estimate_tokens(events, self.config.chars_per_token);
        if estimated <= self.config.token_threshold {
            // Below threshold — reset the guard so a future call in the same
            // cycle can still trigger if tokens grow.
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        // Determine which events to summarize vs. preserve. A held call and everything after
        // it stay, so its approval can still resume it, and the cut never separates a function
        // response from its call.
        let overlap = self.config.overlap_event_count.min(events.len());
        let summarize_end = adk_core::history::balanced_cut(
            events,
            adk_core::held_call_compaction_limit(events, events.len().saturating_sub(overlap)),
        );

        if summarize_end == 0 {
            // All events are in the overlap window — nothing to summarize
            self.compacted_this_cycle.store(false, Ordering::SeqCst);
            return Ok(None);
        }

        let events_to_summarize = &events[..summarize_end];
        let overlap_events = &events[summarize_end..];

        // Call the summarizer — on error, log and return uncompacted
        match self.summarizer.summarize_events(events_to_summarize).await {
            Ok(Some(summary_event)) => {
                // A summary that is not smaller than what it replaces would not shrink the
                // request.
                let summary_tokens = estimate_tokens(
                    std::slice::from_ref(&summary_event),
                    self.config.chars_per_token,
                );
                let replaced_tokens =
                    estimate_tokens(events_to_summarize, self.config.chars_per_token);
                if summary_tokens >= replaced_tokens {
                    tracing::info!(
                        summary.tokens = summary_tokens,
                        replaced.tokens = replaced_tokens,
                        "intra-invocation summary is not smaller than the history it replaces; keeping the history"
                    );
                    return Ok(None);
                }
                let mut compacted = Vec::with_capacity(1 + overlap_events.len());
                compacted.push(summary_event);
                compacted.extend_from_slice(overlap_events);
                Ok(Some(compacted))
            }
            Ok(None) => {
                // Summarizer returned None (e.g., empty input) — no compaction
                Ok(None)
            }
            Err(e) => {
                // A summarizer's model error can echo the history it was given.
                tracing::warn!(
                    error = %e.diagnostics(),
                    "intra-invocation compaction failed, continuing with uncompacted history"
                );
                Ok(None)
            }
        }
    }

    /// Reset the per-cycle guard. Call this at the start of each LLM call.
    pub fn reset_cycle(&self) {
        self.compacted_this_cycle.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        Content, EventActions, EventCompaction, FunctionResponseData, Part,
        ToolConfirmationRequest, confirmation_pending_response, held_tool_calls,
    };
    use async_trait::async_trait;
    use serde_json::json;

    struct Summary;

    #[async_trait]
    impl BaseEventsSummarizer for Summary {
        async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>> {
            let mut event = Event::new("compaction");
            event.actions = EventActions {
                compaction: Some(EventCompaction {
                    start_timestamp: events[0].timestamp,
                    end_timestamp: events[events.len() - 1].timestamp,
                    compacted_content: Content::new("model").with_text("summary"),
                }),
                ..Default::default()
            };
            Ok(Some(event))
        }
    }

    fn event(author: &str, parts: Vec<Part>) -> Event {
        let mut event = Event::new("inv-1");
        event.author = author.to_string();
        event.set_content(Content { role: "model".to_string(), parts });
        event
    }

    #[tokio::test]
    async fn a_held_call_and_the_events_after_it_are_never_summarized() {
        let mut events: Vec<Event> = (0..6)
            .map(|index| {
                event("user", vec![Part::Text { text: format!("message {index} ").repeat(50) }])
            })
            .collect();
        events.push(event(
            "ops",
            vec![Part::FunctionCall {
                name: "transfer".to_string(),
                args: json!({ "amount": 10 }),
                id: Some("c1".to_string()),
                thought_signature: None,
            }],
        ));
        events.push(event(
            "ops",
            vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "transfer",
                    confirmation_pending_response("held"),
                ),
                id: Some("c1".to_string()),
                annotations: None,
            }],
        ));
        let mut hold = Event::new("inv-1");
        hold.author = "ops".to_string();
        hold.actions.tool_confirmation = Some(ToolConfirmationRequest {
            tool_name: "transfer".to_string(),
            function_call_id: Some("c1".to_string()),
            args: json!({ "amount": 10 }),
            acting: None,
            origin: None,
        });
        events.push(hold);
        let start = chrono::Utc::now();
        for (index, event) in events.iter_mut().enumerate() {
            event.timestamp = start + chrono::Duration::milliseconds(index as i64);
        }

        let compactor = IntraInvocationCompactor::new(
            IntraCompactionConfig {
                token_threshold: 1,
                overlap_event_count: 0,
                chars_per_token: 4,
            },
            Arc::new(Summary),
        );
        let compacted = compactor.maybe_compact(&events).await.unwrap().expect("compacted");
        assert_eq!(compacted.len(), 4, "the summary, then the held call's three events");
        assert_eq!(held_tool_calls(&compacted, "ops", "").len(), 1);
    }

    fn call_and_result(id: &str) -> [Event; 2] {
        [
            event(
                "desk",
                vec![Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: json!({}),
                    id: Some(id.to_string()),
                    thought_signature: None,
                }],
            ),
            event(
                "desk",
                vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new("lookup", json!({"ok": true})),
                    id: Some(id.to_string()),
                    annotations: None,
                }],
            ),
        ]
    }

    #[tokio::test]
    async fn the_kept_events_never_start_on_a_result_whose_call_was_summarized() {
        let mut events = vec![event("user", vec![Part::Text { text: "background ".repeat(100) }])];
        events.extend(call_and_result("c1"));
        events.push(event("desk", vec![Part::Text { text: "a1".to_string() }]));
        events.push(event("user", vec![Part::Text { text: "q2".to_string() }]));

        // Keeping three events would cut between the call and its result.
        let compactor = IntraInvocationCompactor::new(
            IntraCompactionConfig {
                token_threshold: 1,
                overlap_event_count: 3,
                chars_per_token: 4,
            },
            Arc::new(Summary),
        );
        let compacted = compactor.maybe_compact(&events).await.unwrap().expect("compacted");
        assert_eq!(compacted.len(), 5, "the summary, then the call, its result, a1 and q2");
        assert_eq!(compacted[1].id, events[1].id);
    }

    #[tokio::test]
    async fn a_summary_that_does_not_shrink_the_history_is_not_used() {
        let mut events = vec![event("user", vec![Part::Text { text: "q".to_string() }])];
        events.push(event("desk", vec![Part::Text { text: "a".to_string() }]));
        events.push(event("user", vec![Part::Text { text: "q2".to_string() }]));
        let compactor = IntraInvocationCompactor::new(
            IntraCompactionConfig {
                token_threshold: 0,
                overlap_event_count: 1,
                chars_per_token: 1,
            },
            Arc::new(Summary),
        );
        // "summary" is longer than the two one-character events it would replace.
        assert!(compactor.maybe_compact(&events).await.unwrap().is_none());
    }
}
