//! Property-based tests for intra-invocation context compaction.
//!
//! Tests the token estimator, compaction trigger logic, overlap preservation,
//! and at-most-once-per-cycle guard.

use adk_core::intra_compaction::{IntraCompactionConfig, estimate_tokens};
use adk_core::{BaseEventsSummarizer, Content, Event, EventActions, EventCompaction};
use adk_core::{AdkError, ErrorComponent};
use adk_runner::IntraInvocationCompactor;
use async_trait::async_trait;
use proptest::prelude::*;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Mock summarizer for testing
// ---------------------------------------------------------------------------

/// A mock summarizer that returns a simple summary event containing
/// a text description of how many events were summarized.
struct MockSummarizer;

#[async_trait]
impl BaseEventsSummarizer for MockSummarizer {
    async fn summarize_events(&self, events: &[Event]) -> adk_core::Result<Option<Event>> {
        if events.is_empty() {
            return Ok(None);
        }

        let summary_text = format!("Summary of {} events", events.len());
        let summary_content = Content::new("model").with_text(summary_text);

        let start_timestamp = events.first().map(|e| e.timestamp).unwrap_or_default();
        let end_timestamp = events.last().map(|e| e.timestamp).unwrap_or_default();

        let compaction = EventCompaction {
            start_timestamp,
            end_timestamp,
            compacted_content: summary_content.clone(),
        };

        let mut event = Event::new("compaction");
        event.author = "system".to_string();
        event.llm_response.content = Some(summary_content);
        event.actions = EventActions {
            compaction: Some(compaction),
            ..Default::default()
        };

        Ok(Some(event))
    }
}

/// A mock summarizer that always returns an error.
struct FailingSummarizer;

#[async_trait]
impl BaseEventsSummarizer for FailingSummarizer {
    async fn summarize_events(&self, _events: &[Event]) -> adk_core::Result<Option<Event>> {
        Err(AdkError::internal(
            ErrorComponent::Agent,
            "compaction.mock_failure",
            "mock summarizer failure",
        ))
    }
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generate a simple text event with a given text.
fn make_text_event(text: &str) -> Event {
    let mut event = Event::new("inv-test");
    event.set_content(Content::new("user").with_text(text));
    event
}

/// Strategy for generating a list of text events with known character counts.
fn arb_text_events() -> impl Strategy<Value = Vec<(String, Event)>> {
    prop::collection::vec("[a-zA-Z0-9 ]{1,100}", 1..20).prop_map(|texts| {
        texts
            .into_iter()
            .map(|text| {
                let event = make_text_event(&text);
                (text, event)
            })
            .collect()
    })
}

/// Strategy for generating chars_per_token ratio (must be > 0).
fn arb_chars_per_token() -> impl Strategy<Value = u32> {
    1..=20u32
}

/// Strategy for generating a token threshold.
fn arb_token_threshold() -> impl Strategy<Value = u64> {
    1..=10_000u64
}

/// Strategy for generating overlap event count.
fn arb_overlap() -> impl Strategy<Value = usize> {
    0..=10usize
}

// ---------------------------------------------------------------------------
// Property 12: Token Estimator Correctness
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 12: Token Estimator Correctness**
    ///
    /// *For any* list of events and *for any* `chars_per_token` ratio > 0,
    /// `estimate_tokens(events, ratio)` SHALL return a value equal to the total
    /// character count of all text content in the events divided by `ratio`
    /// (integer division).
    ///
    /// **Validates: Requirements 12.4**
    #[test]
    fn prop_token_estimator_correctness(
        text_events in arb_text_events(),
        chars_per_token in arb_chars_per_token(),
    ) {
        let events: Vec<Event> = text_events.iter().map(|(_, e)| e.clone()).collect();

        // Compute expected: sum of text lengths / chars_per_token
        let total_chars: u64 = text_events.iter()
            .map(|(text, _)| text.len() as u64)
            .sum();
        let expected = total_chars / chars_per_token as u64;

        let actual = estimate_tokens(&events, chars_per_token);
        prop_assert_eq!(actual, expected);
    }
}

// ---------------------------------------------------------------------------
// Property 13: Compaction Triggers at Threshold
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 13: Compaction Triggers at Threshold**
    ///
    /// *For any* list of events and *for any* `IntraCompactionConfig`,
    /// `maybe_compact` SHALL return `Some(compacted_events)` if and only if
    /// `estimate_tokens(events, config.chars_per_token) > config.token_threshold`.
    ///
    /// **Validates: Requirements 12.1**
    #[test]
    fn prop_compaction_triggers_at_threshold(
        text_events in arb_text_events(),
        chars_per_token in arb_chars_per_token(),
        token_threshold in arb_token_threshold(),
        overlap in arb_overlap(),
    ) {
        let events: Vec<Event> = text_events.iter().map(|(_, e)| e.clone()).collect();

        let config = IntraCompactionConfig {
            token_threshold,
            overlap_event_count: overlap,
            chars_per_token,
        };

        let estimated = estimate_tokens(&events, chars_per_token);
        let should_compact = estimated > token_threshold;

        let compactor = IntraInvocationCompactor::new(
            config.clone(),
            Arc::new(MockSummarizer),
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = rt.block_on(compactor.maybe_compact(&events)).unwrap();

        if should_compact && overlap < events.len() {
            // Should have compacted (unless all events are in overlap window)
            let summarize_end = events.len().saturating_sub(overlap.min(events.len()));
            if summarize_end > 0 {
                prop_assert!(result.is_some(),
                    "Expected compaction: estimated={estimated} > threshold={token_threshold}, overlap={overlap}, events={}", events.len());
            }
        } else {
            prop_assert!(result.is_none(),
                "Expected no compaction: estimated={estimated}, threshold={token_threshold}");
        }
    }
}

// ---------------------------------------------------------------------------
// Property 14: Compaction Preserves Overlap Events
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 14: Compaction Preserves Overlap Events**
    ///
    /// *For any* list of events where compaction is triggered, and *for any*
    /// `overlap_event_count` N ≤ len(events), the last N events in the compacted
    /// result SHALL be identical to the last N events in the original list.
    ///
    /// **Validates: Requirements 12.3**
    #[test]
    fn prop_compaction_preserves_overlap(
        // Generate enough text to exceed a low threshold
        num_events in 3..15usize,
        overlap in 1..=5usize,
    ) {
        // Create events with enough text to exceed threshold
        let events: Vec<Event> = (0..num_events)
            .map(|i| {
                let text = format!("Event number {} with some padding text to increase character count significantly for testing purposes", i);
                make_text_event(&text)
            })
            .collect();

        let effective_overlap = overlap.min(num_events);

        let config = IntraCompactionConfig {
            token_threshold: 1, // Very low threshold to ensure compaction triggers
            overlap_event_count: effective_overlap,
            chars_per_token: 4,
        };

        let compactor = IntraInvocationCompactor::new(
            config,
            Arc::new(MockSummarizer),
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let result = rt.block_on(compactor.maybe_compact(&events)).unwrap();

        if let Some(compacted) = result {
            // The last N events should be preserved
            let original_tail = &events[events.len() - effective_overlap..];
            let compacted_tail = &compacted[compacted.len() - effective_overlap..];

            prop_assert_eq!(original_tail.len(), compacted_tail.len(),
                "Overlap count mismatch");

            for (orig, comp) in original_tail.iter().zip(compacted_tail.iter()) {
                prop_assert_eq!(&orig.id, &comp.id,
                    "Overlap event ID mismatch");
            }

            // First event should be the summary
            prop_assert_eq!(&compacted[0].author, "system",
                "First event should be the summary from the summarizer");
        }
    }
}

// ---------------------------------------------------------------------------
// Property 15: Compaction At Most Once Per Cycle
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: competitive-parity-v070, Property 15: Compaction At Most Once Per Cycle**
    ///
    /// *For any* list of events above the compaction threshold, calling
    /// `maybe_compact` twice within the same cycle (without calling `reset_cycle`)
    /// SHALL return `Some` on the first call and `None` on the second call.
    ///
    /// **Validates: Requirements 12.7**
    #[test]
    fn prop_compaction_at_most_once_per_cycle(
        num_events in 3..15usize,
    ) {
        let events: Vec<Event> = (0..num_events)
            .map(|i| {
                let text = format!("Event {} with enough text to exceed the very low threshold we set for testing", i);
                make_text_event(&text)
            })
            .collect();

        let config = IntraCompactionConfig {
            token_threshold: 1, // Very low to ensure compaction triggers
            overlap_event_count: 1,
            chars_per_token: 4,
        };

        let compactor = IntraInvocationCompactor::new(
            config,
            Arc::new(MockSummarizer),
        );

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // First call should compact
        let first = rt.block_on(compactor.maybe_compact(&events)).unwrap();
        prop_assert!(first.is_some(), "First call should trigger compaction");

        // Second call without reset should NOT compact
        let second = rt.block_on(compactor.maybe_compact(&events)).unwrap();
        prop_assert!(second.is_none(), "Second call without reset should not compact");

        // After reset, should compact again
        compactor.reset_cycle();
        let third = rt.block_on(compactor.maybe_compact(&events)).unwrap();
        prop_assert!(third.is_some(), "After reset_cycle, compaction should trigger again");
    }
}

// ---------------------------------------------------------------------------
// Additional unit test: summarizer error handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_summarizer_error_returns_none() {
    let events: Vec<Event> = (0..5)
        .map(|i| make_text_event(&format!("Event {i} with enough text to exceed threshold")))
        .collect();

    let config = IntraCompactionConfig {
        token_threshold: 1,
        overlap_event_count: 1,
        chars_per_token: 4,
    };

    let compactor = IntraInvocationCompactor::new(config, Arc::new(FailingSummarizer));

    // Should return None (not propagate the error)
    let result = compactor.maybe_compact(&events).await.unwrap();
    assert!(result.is_none(), "Summarizer error should result in None (uncompacted history)");
}
